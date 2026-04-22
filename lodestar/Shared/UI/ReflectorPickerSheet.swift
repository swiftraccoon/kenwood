// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Modal sheet for choosing a reflector. Opens quickly: default view
/// is the ~12-entry featured list. "Show all" expands to the full ~200+
/// Pi-Star list, rendered with a lazy `List` so scrolling stays smooth.
struct ReflectorPickerSheet: View {
    @Bindable var coordinator: ReflectorCoordinator
    @Environment(\.dismiss) private var dismiss

    @State private var search: String = ""
    @State private var showAll: Bool = false
    @State private var protocolFilter: ReflectorProtocol? = nil

    private let all: [Reflector] = defaultReflectors()
    private let modules: [String] = ["A", "B", "C", "D", "E"]

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                settingsBar
                Divider()
                list
            }
            .navigationTitle("Choose reflector")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .searchable(text: $search, prompt: "Search 200+ reflectors")
            .onChange(of: search) { _, newValue in
                if !newValue.isEmpty { showAll = true }
            }
        }
        #if os(macOS)
        .frame(minWidth: 480, idealWidth: 520, minHeight: 520, idealHeight: 600)
        #endif
    }

    // MARK: - Sections

    private var settingsBar: some View {
        VStack(spacing: 8) {
            HStack {
                Label {
                    TextField("Callsign (e.g. W1AW)", text: $coordinator.callsign)
                        .textFieldStyle(.roundedBorder)
                        .autocorrectionDisabled()
                        #if os(iOS)
                        .textInputAutocapitalization(.characters)
                        #endif
                } icon: {
                    Image(systemName: "person.fill")
                        .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 12) {
                HStack(spacing: 4) {
                    Text("Local").font(.caption).foregroundStyle(.secondary)
                    Picker("", selection: $coordinator.localModule) {
                        ForEach(modules, id: \.self) { Text($0).tag($0) }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 64)
                }
                HStack(spacing: 4) {
                    Text("Reflector").font(.caption).foregroundStyle(.secondary)
                    Picker("", selection: $coordinator.reflectorModule) {
                        ForEach(modules, id: \.self) { Text($0).tag($0) }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 64)
                }
                Spacer()
                protocolPicker
            }
        }
        .padding()
    }

    private var protocolPicker: some View {
        Picker("Protocol", selection: $protocolFilter) {
            Text("All").tag(Optional<ReflectorProtocol>.none)
            Text("DPlus").tag(Optional<ReflectorProtocol>.some(.dPlus))
            Text("DExtra").tag(Optional<ReflectorProtocol>.some(.dExtra))
            Text("DCS").tag(Optional<ReflectorProtocol>.some(.dcs))
        }
        .pickerStyle(.segmented)
        .frame(maxWidth: 260)
    }

    private var list: some View {
        List {
            if let err = coordinator.lastError {
                Section {
                    Label(err, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.callout)
                }
            }

            if canShowFeatured {
                Section {
                    ForEach(displayed(), id: \.name) { r in row(r) }
                } header: {
                    HStack {
                        Label("Featured", systemImage: "star.fill")
                            .foregroundStyle(.yellow)
                        Spacer()
                        Button {
                            withAnimation(.snappy) { showAll = true }
                        } label: {
                            Text("Show all \(all.count)")
                                .font(.caption)
                        }
                        .buttonStyle(.borderless)
                    }
                } footer: {
                    Text("XRF and DCS don't need callsign registration. REF (DPlus) requires registering at dstargateway.org.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else {
                Section {
                    ForEach(displayed(), id: \.name) { r in row(r) }
                } header: {
                    HStack {
                        Text("\(displayed().count) reflector\(displayed().count == 1 ? "" : "s")")
                        Spacer()
                        if search.isEmpty {
                            Button {
                                withAnimation(.snappy) { showAll = false }
                            } label: {
                                Text("Featured only")
                                    .font(.caption)
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                }
            }
        }
        #if os(macOS)
        .listStyle(.inset)
        #else
        .listStyle(.insetGrouped)
        #endif
    }

    // MARK: - Helpers

    private var canShowFeatured: Bool {
        !showAll && search.isEmpty && protocolFilter == nil
    }

    private func displayed() -> [Reflector] {
        if canShowFeatured {
            return FeaturedReflectors.resolve(from: all)
        }
        return all.filter { r in
            let byProto = protocolFilter == nil || protocolFilter == r.protocol
            let bySearch = search.isEmpty
                || r.name.localizedCaseInsensitiveContains(search)
                || r.host.localizedCaseInsensitiveContains(search)
            return byProto && bySearch
        }
    }

    private func row(_ r: Reflector) -> some View {
        Button {
            Task {
                await coordinator.connect(to: r)
                dismiss()
            }
        } label: {
            HStack(spacing: 12) {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(r.protocol.accentColor.opacity(0.18))
                    .frame(width: 34, height: 34)
                    .overlay(
                        Image(systemName: r.protocol.sfSymbol)
                            .foregroundStyle(r.protocol.accentColor)
                    )

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(r.name).font(.body.bold())
                        Text(r.protocol.displayName)
                            .font(.caption2.bold())
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(r.protocol.accentColor.opacity(0.15))
                            .foregroundStyle(r.protocol.accentColor)
                            .clipShape(Capsule())
                    }
                    Text("\(r.host):\(String(r.port))")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }

                Spacer()

                Image(systemName: "arrow.up.right.circle")
                    .foregroundStyle(.blue)
            }
            .padding(.vertical, 2)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(coordinator.isBusy || coordinator.callsign.isEmpty)
        .accessibilityLabel("Connect to \(r.name) on \(r.host)")
    }
}
