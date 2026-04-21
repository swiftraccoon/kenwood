// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct ContentView: View {
    @State private var coordinator = TransportCoordinator()

    private let appVersion: String =
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"

    private let coreVersion: String = version()

    var body: some View {
        NavigationStack {
            Group {
                switch coordinator.state {
                case .disconnected, .failed:
                    DevicePickerView(coordinator: coordinator)
                case .connecting, .connected:
                    ConnectionView(coordinator: coordinator)
                }
            }
            .navigationTitle("Lodestar")
            .toolbar {
                ToolbarItem(placement: .status) {
                    Text("app v\(appVersion) · core v\(coreVersion)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

#Preview("Disconnected") {
    ContentView()
}
