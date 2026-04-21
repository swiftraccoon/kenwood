// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct DevicePickerView: View {
    let coordinator: TransportCoordinator

    var body: some View {
        VStack(spacing: 16) {
            Text("Select Radio")
                .font(.title2)
                .bold()

            if coordinator.availableDevices.isEmpty {
                ContentUnavailableView {
                    Label("No paired Bluetooth devices", systemImage: "antenna.radiowaves.left.and.right.slash")
                } description: {
                    #if os(macOS)
                    Text("Pair a TH-D75 via macOS Bluetooth settings, then refresh.")
                    #else
                    Text("Bluetooth Classic SPP isn't available on iOS or iPadOS. Run the macOS build, or wait for the USB-C CDC transport in a later phase.")
                    #endif
                }
            } else {
                List(coordinator.availableDevices) { dev in
                    Button {
                        coordinator.select(dev)
                        Task { await coordinator.connect() }
                    } label: {
                        HStack {
                            VStack(alignment: .leading) {
                                Text(dev.name)
                                    .font(.headline)
                                Text(dev.address)
                                    .font(.caption.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            if coordinator.selectedDevice?.id == dev.id {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundStyle(.green)
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(dev.name), address \(dev.address)")
                }
            }

            Button {
                coordinator.refreshPairedDevices()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.borderedProminent)
        }
        .padding()
        .onAppear {
            coordinator.refreshPairedDevices()
        }
    }
}
