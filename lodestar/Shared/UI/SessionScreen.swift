// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Single primary screen. Shows the full chain (radio, reflector,
/// live stream) as one coherent flow. The relay runs automatically
/// when preconditions are met; the user never toggles it.
struct SessionScreen: View {
    let session: SessionCoordinator

    @State private var showPicker = false
    @State private var showDevicePicker = false
    @State private var showHeardHistory = false
    @State private var showSettings = false
    @State private var showDiagnosticsPanel = false
    @State private var canvasWidth: CGFloat = 0

    private var transport: TransportCoordinator { session.transport }
    private var reflector: ReflectorCoordinator { session.reflector }
    private var relay: RelayCoordinator { session.relay }

    var body: some View {
        SessionCanvas(
            session: session,
            showDiagnosticsPanel: $showDiagnosticsPanel,
            canvasWidth: $canvasWidth,
            onConnectRadio: { showDevicePicker = true },
            onChooseReflector: { showPicker = true },
            onShowAllHeard: { showHeardHistory = true }
        )
        .sheet(isPresented: $showPicker) {
            ReflectorPickerSheet(coordinator: reflector)
        }
        .sheet(isPresented: $showDevicePicker) {
            DevicePickerSheet(coordinator: transport)
        }
        .sheet(isPresented: $showHeardHistory) {
            HeardHistorySheet(coordinator: reflector)
        }
        .toolbar {
            if canvasWidth >= SessionCanvas.wideBreakpoint {
                ToolbarItem {
                    Toggle(isOn: $showDiagnosticsPanel) {
                        Image(systemName: "stethoscope")
                    }
                    .accessibilityLabel("Radio diagnostics")
                }
            }
            #if os(iOS)
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    showSettings = true
                } label: {
                    Image(systemName: "gearshape")
                }
                .accessibilityLabel("Settings")
            }
            #endif
        }
        #if os(iOS)
        .sheet(isPresented: $showSettings) {
            SettingsSheet()
        }
        #endif
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

}

/// Firmware-qualified setup for the selected host interface and terminal mode.
struct McpCard: View {
    let transport: TransportCoordinator

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Label("Enable Reflector Terminal Mode", systemImage: "gearshape.2")
                    .font(.headline)
                Text("Verifies the exact TH-D75 model and firmware, routes Menu 985 "
                     + "to the connected transport, and enables Menu 650 "
                     + "in one programming session.")
                    .font(.callout)
                    .foregroundStyle(.secondary)

                Button {
                    Task {
                        await transport.enableReflectorTerminalMode()
                    }
                } label: {
                    Label("Enable Terminal Mode", systemImage: "arrow.up.right.circle.fill")
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
        case .succeeded:
            VStack(alignment: .leading, spacing: 4) {
                Label("Reflector Terminal Mode enabled", systemImage: "checkmark.seal.fill")
                    .foregroundStyle(.green)
                    .font(.callout)
                Text("Menu 985 routes DV Gateway to this connection, Menu 650 is Reflector "
                     + "Terminal Mode, and the same radio has reconnected with MMDVM framing.")
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

/// Device-picker sheet; mirrors the reflector sheet pattern.
private struct DevicePickerSheet: View {
    let coordinator: TransportCoordinator
    @Environment(\.dismiss) private var dismiss
    @State private var connectionTask: Task<Void, Never>?

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
            .onDisappear {
                connectionTask?.cancel()
                connectionTask = nil
                coordinator.cancelConnectionAttempt()
            }
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
        ToolbarItem {
            Button {
                coordinator.refreshPairedDevices()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
        }
    }

    #if os(macOS)
    @ViewBuilder
    private var macBody: some View {
        if coordinator.availableDevices.isEmpty {
            ContentUnavailableView {
                Label("No paired devices", systemImage: "antenna.radiowaves.left.and.right.slash")
            } description: {
                Text("Pair your TH-D75 in **System Settings → Bluetooth** "
                     + "(Menu 934 enables pairing), then refresh.")
            } actions: {
                Button("Refresh") {
                    coordinator.refreshPairedDevices()
                }
                .buttonStyle(.borderedProminent)
            }
        } else {
            List {
                Section {
                    ForEach(coordinator.availableDevices) { dev in
                        deviceButton(dev)
                    }
                } header: {
                    Text("Paired Bluetooth Devices")
                } footer: {
                    Text("Choose the radio. Lodestar opens only that exact Bluetooth address "
                         + "and accepts it only after exact CAT ID TH-D75 or complete MMDVM "
                         + "GetVersion framing is proved.")
                }
            }
        }
    }
    #endif

    #if os(iOS)
    /// iPad direct-radio access rides the USB-CDC DriverKit transport:
    /// the device list is non-empty exactly when the dext service is
    /// registered: radio plugged in over USB-C AND the Lodestar driver
    /// enabled in Settings. (DriverKit never runs in the Simulator, so
    /// the Simulator always shows the empty state.)
    @ViewBuilder
    private var ipadBody: some View {
        if coordinator.availableDevices.isEmpty {
            ContentUnavailableView {
                Label("No TH-D75 found over USB", systemImage: "cable.connector")
            } description: {
                Text("Plug the radio into the iPad's USB-C port with a data-capable cable, and enable the Lodestar driver in **Settings → General → Drivers**. Reflectors work without a radio (TX/RX over IP).")
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

    private func deviceButton(_ dev: BluetoothDevice) -> some View {
        Button {
            coordinator.select(dev)
            connectionTask?.cancel()
            connectionTask = Task {
                await coordinator.connect()
                guard !Task.isCancelled, coordinator.state == .connected else { return }
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
