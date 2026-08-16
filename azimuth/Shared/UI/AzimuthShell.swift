// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

struct AzimuthShell: View {
    @Environment(AzimuthSceneModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            #if os(macOS)
            macShell
            #else
            ipadShell
            #endif
        }
        .task { model.activate() }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                #if os(iOS)
                model.handleScenePhaseBackground()
                #endif
            case .active:
                #if os(iOS)
                model.handleScenePhaseActive()
                #else
                model.activate()
                #endif
            case .inactive:
                // App switcher, system overlays, and brief interruptions do
                // not suspend the app and should not bounce a healthy link.
                break
            @unknown default:
                break
            }
        }
        .alert(
            model.catRecoveryAlert?.title ?? "USB-C Is in MMDVM Mode",
            isPresented: Binding(
                get: { model.catRecoveryAlert != nil },
                set: { if !$0 { model.dismissCATRecoveryAlert() } }
            )
        ) {
            if model.catRecoveryAlert?.automaticRecoveryAvailable == true {
                Button("Restore CAT Automatically") {
                    Task { await model.restoreCATFromUSBMMDVM() }
                }
            }
            if model.catRecoveryAlert?.isRecoveryOffer == true {
                Button("Not Now", role: .cancel) {
                    model.dismissCATRecoveryAlert()
                }
            } else {
                Button("OK", role: .cancel) {
                    model.dismissCATRecoveryAlert()
                }
            }
        } message: {
            Text(model.catRecoveryAlert?.message ?? "CAT control is unavailable.")
        }
    }

    #if os(macOS)
    private var macShell: some View {
        @Bindable var model = model
        return NavigationSplitView {
            VStack(spacing: 0) {
                AzimuthWordmark()
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 18)

                List(AzimuthRoute.allCases, selection: $model.route) { route in
                    Label(route.title, systemImage: route.symbol)
                        .font(.body.weight(.medium))
                        .tag(route)
                        .padding(.vertical, 3)
                }
                .listStyle(.sidebar)

                sidebarStatus
                    .padding(12)
            }
            .navigationSplitViewColumnWidth(min: 190, ideal: 225, max: 280)
            .background(.ultraThinMaterial)
        } detail: {
            NavigationStack {
                routeContent(model.route)
                    .navigationTitle(model.route.title)
            }
        }
        .navigationSplitViewStyle(.balanced)
    }

    private var sidebarStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            Divider()
            HStack(spacing: 8) {
                Circle()
                    .fill(model.radioState.connection.isConnected
                        ? AzimuthPalette.signal : Color.secondary.opacity(0.45))
                    .frame(width: 8, height: 8)
                VStack(alignment: .leading, spacing: 1) {
                    Text(sidebarConnectionTitle)
                        .font(.caption.weight(.semibold))
                    Text(sidebarConnectionDetail)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
    }

    private var sidebarConnectionTitle: String {
        switch model.radioState.connection {
        case .connected(let device, _): return device
        case .connecting: return "Connecting…"
        case .failed: return "Connection failed"
        case .disconnected: return "No radio connected"
        }
    }

    private var sidebarConnectionDetail: String {
        switch model.radioState.connection {
        case .connected(_, let transport): return transport.uppercased()
        case .connecting: return "USB CONTROL"
        case .failed: return "CHECK RADIO"
        case .disconnected: return "USB NOT CONNECTED"
        }
    }
    #endif

    #if os(iOS)
    private var ipadShell: some View {
        @Bindable var model = model
        return TabView(selection: $model.route) {
            Tab("Radio", systemImage: AzimuthRoute.radio.symbol, value: .radio) {
                NavigationStack {
                    RadioWorkspace()
                        .navigationTitle("TH-D75")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }

            Tab("APRS", systemImage: AzimuthRoute.aprs.symbol, value: .aprs) {
                NavigationStack {
                    APRSWorkspace()
                        .navigationTitle("APRS")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }

            Tab("IF-DSP", systemImage: AzimuthRoute.ifDSP.symbol, value: .ifDSP) {
                NavigationStack {
                    IFDSPWorkspace()
                        .navigationTitle("IF-DSP")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }

            Tab("Settings", systemImage: AzimuthRoute.settings.symbol, value: .settings) {
                NavigationStack {
                    SettingsCatalogView()
                        .navigationTitle("Settings")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }

            Tab("Assistant", systemImage: AzimuthRoute.assistant.symbol, value: .assistant) {
                NavigationStack {
                    AssistantView()
                        .navigationTitle("Assistant")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }

            Tab("Learn", systemImage: AzimuthRoute.learn.symbol, value: .learn) {
                NavigationStack {
                    LearnView()
                        .navigationTitle("Learn")
                        .navigationBarTitleDisplayMode(.inline)
                }
            }
        }
        .tabViewStyle(.tabBarOnly)
        .accessibilityIdentifier("azimuth.navigation")
    }
    #endif

    @ViewBuilder
    private func routeContent(_ route: AzimuthRoute) -> some View {
        switch route {
        case .radio: RadioWorkspace()
        case .aprs: APRSWorkspace()
        case .ifDSP: IFDSPWorkspace()
        case .settings: SettingsCatalogView()
        case .assistant: AssistantView()
        case .learn: LearnView()
        }
    }
}
