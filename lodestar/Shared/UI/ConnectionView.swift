// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct ConnectionView: View {
    let coordinator: TransportCoordinator

    var body: some View {
        VStack(spacing: 20) {
            header

            Group {
                switch coordinator.state {
                case .connected:
                    sendSection
                case .connecting:
                    ProgressView("Connecting…")
                case .failed(let msg):
                    Label("Failed: \(msg)", systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .accessibilityLabel("Connection failed: \(msg)")
                case .disconnected:
                    Text("Disconnected.")
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.vertical, 8)

            if !coordinator.lastResponseText.isEmpty {
                responseBox
            }

            Spacer()

            Button(role: .destructive) {
                Task { await coordinator.disconnect() }
            } label: {
                Label("Disconnect", systemImage: "xmark.circle")
            }
            .buttonStyle(.bordered)
        }
        .padding()
    }

    private var header: some View {
        VStack(spacing: 4) {
            Text(coordinator.selectedDevice?.name ?? "—")
                .font(.title2)
                .bold()
            Text(coordinator.selectedDevice?.address ?? "")
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
        }
    }

    private var sendSection: some View {
        VStack(spacing: 12) {
            Label("Connected", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
            Button {
                Task { await coordinator.sendIdentify() }
            } label: {
                Label("Send ID", systemImage: "paperplane")
            }
            .buttonStyle(.borderedProminent)
            .disabled(coordinator.isBusy)
        }
    }

    private var responseBox: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Last response")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(coordinator.lastResponseText)
                .font(.body.monospaced())
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.secondary.opacity(0.1))
                .cornerRadius(8)
        }
        .accessibilityElement(children: .combine)
    }
}
