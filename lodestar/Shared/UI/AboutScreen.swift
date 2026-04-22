// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct AboutScreen: View {
    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
    }

    private var coreVersion: String {
        Lodestar.version()
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                appHero
                facts
                platformNote
                attributions
            }
            .padding()
            .frame(maxWidth: 540)
        }
    }

    private var appHero: some View {
        VStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 22, style: .continuous)
                    .fill(.blue.opacity(0.12))
                    .frame(width: 96, height: 96)
                Image(systemName: "dot.radiowaves.left.and.right")
                    .font(.system(size: 48, weight: .medium))
                    .foregroundStyle(.blue)
            }
            Text("Lodestar").font(.largeTitle.bold())
            Text("D-STAR gateway for the Kenwood TH-D75")
                .font(.title3)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 16)
    }

    private var facts: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                row("App version", appVersion)
                row("Core (Rust) version", coreVersion)
                row("Repository", "github.com/swiftraccoon/kenwood")
                row("License", "GPL-2.0-or-later / GPL-3.0-or-later")
            }
        }
    }

    private var platformNote: some View {
        GroupBox("Platform") {
            VStack(alignment: .leading, spacing: 6) {
                #if os(macOS)
                Label("Running natively on macOS", systemImage: "macbook")
                    .font(.callout.bold())
                Text("Bluetooth, reflector connect, and MCP programming all work. The same app bundle runs on iPhone and iPad — but without Bluetooth access there until Apple opens BT Classic SPP.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                #else
                Label("Running on iOS / iPadOS", systemImage: "iphone")
                    .font(.callout.bold())
                Text("Reflector connect works over Wi-Fi / cellular. Bluetooth to a TH-D75 requires the macOS build (or a future USB-C transport on iPhone 15+ and iPads).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                #endif
            }
        }
    }

    private var attributions: some View {
        GroupBox("Attribution") {
            VStack(alignment: .leading, spacing: 6) {
                Text("Reflector hosts list: ircDDBGateway (G4KLX).")
                    .font(.caption)
                Text("D-STAR protocol codec: dstar-gateway-core (swiftraccoon).")
                    .font(.caption)
                Text("MMDVM protocol: MMDVMHost (G4KLX) via mmdvm-core.")
                    .font(.caption)
                Text("AMBE decoder: mbelib-rs.")
                    .font(.caption)
            }
            .foregroundStyle(.secondary)
        }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
            Spacer()
            Text(value).font(.body.monospaced()).foregroundStyle(.secondary)
        }
    }
}
