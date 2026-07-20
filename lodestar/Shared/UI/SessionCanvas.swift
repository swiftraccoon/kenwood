// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Adaptive session layout. Wide (≥ 700 pt of actual available width —
/// never device idiom): the station map is the full-bleed content
/// layer with a 380 pt rail of material cards whose composition comes
/// from `RailState.derive`. Narrow: the classic single scrolling
/// column with the map demoted to a fixed-height card. Reflow is
/// non-destructive in both directions — all state lives above the
/// branch, so resizing only re-composes.
struct SessionCanvas: View {
    static let wideBreakpoint: CGFloat = 700
    private static let railWidth: CGFloat = 380

    let session: SessionCoordinator
    @Binding var showDiagnosticsPanel: Bool
    @Binding var canvasWidth: CGFloat
    let onConnectRadio: () -> Void
    let onChooseReflector: () -> Void
    let onShowAllHeard: () -> Void

    @State private var manualChainExpanded = false
    @State private var selectedStationID: String?

    private var transport: TransportCoordinator { session.transport }
    private var reflector: ReflectorCoordinator { session.reflector }
    private var relay: RelayCoordinator { session.relay }

    private var railState: RailState {
        RailState.derive(RailState.Inputs(
            transport: transport.state,
            radioMode: transport.radioMode,
            mcpStatus: transport.mcpStatus,
            hasProbeError: transport.lastProbeErrorText != nil,
            reflector: reflector.state,
            relay: relay.state,
            streamActive: reflector.currentStream != nil,
            hasHeardHistory: !reflector.recentlyHeard.isEmpty,
            manualChainExpanded: manualChainExpanded
        ))
    }

    var body: some View {
        // The wide/narrow branch MUST be driven synchronously by the
        // width layout hands us (GeometryReader), never by a state
        // round-trip: an `onGeometryChange` observer attached to the
        // branch subtree gets destroyed by the very state write it
        // makes (narrow → wide replaces the subtree), so SwiftUI's
        // layout-loop protection suppresses the update and the initial
        // branch renders forever. The outer observer below only feeds
        // the toolbar's diagnostics toggle — a consumer that cannot
        // affect this view's own layout, so it cannot loop.
        GeometryReader { proxy in
            Group {
                if proxy.size.width >= Self.wideBreakpoint {
                    wide
                } else {
                    narrow
                }
            }
            .frame(width: proxy.size.width, height: proxy.size.height)
        }
        .onGeometryChange(for: CGFloat.self) { proxy in
            proxy.size.width
        } action: { width in
            canvasWidth = width
        }
    }

    // MARK: - Wide

    private var wide: some View {
        ZStack(alignment: .topLeading) {
            StationMapPane(
                heard: reflector.recentlyHeard,
                liveStream: reflector.currentStream,
                dimmed: railState.mapDimmed,
                style: .canvas,
                selectedStationID: $selectedStationID
            )
            .ignoresSafeArea()

            rail

            if showDiagnosticsPanel {
                HStack {
                    Spacer()
                    VStack {
                        DiagnosticsInspector(transport: transport, presentation: .panel)
                        Spacer()
                    }
                }
                .padding()
            }
        }
    }

    private var rail: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ChainPane(
                    session: session,
                    display: railState.chain,
                    onConnectRadio: onConnectRadio,
                    onChooseReflector: onChooseReflector,
                    onToggleExpand: { manualChainExpanded.toggle() }
                )
                .padding(12)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))

                if railState.showsMcpCard {
                    McpCard(transport: transport)
                        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }

                if railState.showsNowTransmitting {
                    NowTransmittingPane(stream: reflector.currentStream, size: .hero)
                        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }

                if reflector.state == .connected, !reflector.monitorAudioEnabled {
                    Button {
                        reflector.monitorAudioEnabled = true
                    } label: {
                        Label("Monitor muted — unmute", systemImage: "speaker.slash")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityHint("Turns reflector audio monitoring back on")
                }

                if railState.showsHeardList {
                    HeardListPane(
                        reflector: reflector,
                        limit: nil,
                        selectedStationID: $selectedStationID,
                        onShowAll: nil
                    )
                    .padding(12)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }
            }
            .padding()
        }
        .frame(width: Self.railWidth)
    }

    // MARK: - Narrow (the classic column, map card added)

    private var narrow: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                narrowHero
                ChainPane(
                    session: session,
                    display: .expanded,
                    onConnectRadio: onConnectRadio,
                    onChooseReflector: onChooseReflector,
                    onToggleExpand: {}
                )
                DiagnosticsInspector(transport: transport, presentation: .card)
                if railState.showsMcpCard {
                    McpCard(transport: transport)
                }
                if reflector.state == .connected {
                    // Always shown once linked, so an idle reflector
                    // still surfaces the chain and reassures that the
                    // link is live. Fields show placeholders while
                    // nobody's transmitting.
                    NowTransmittingPane(stream: reflector.currentStream, size: .regular)
                    if !reflector.monitorAudioEnabled {
                        // Tappable so the muted state is never a dead end —
                        // the menu toggle exists too, but this is the spot
                        // the operator is already looking at.
                        Button {
                            reflector.monitorAudioEnabled = true
                        } label: {
                            Label("Monitor muted — unmute", systemImage: "speaker.slash")
                                .font(.caption)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityHint("Turns reflector audio monitoring back on")
                    }
                    StationMapPane(
                        heard: reflector.recentlyHeard,
                        liveStream: reflector.currentStream,
                        dimmed: railState.mapDimmed,
                        style: .card,
                        selectedStationID: $selectedStationID
                    )
                }
                if reflector.state == .connected || !reflector.recentlyHeard.isEmpty {
                    HeardListPane(
                        reflector: reflector,
                        limit: reflector.inlineHeardLimit,
                        selectedStationID: $selectedStationID,
                        onShowAll: onShowAllHeard
                    )
                }
            }
            .padding()
            .frame(maxWidth: 640, alignment: .leading)
        }
    }

    private var narrowHero: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Circle()
                    .fill(ChainPane.heroTint(transport: transport, reflector: reflector, relay: relay))
                    .frame(width: 12, height: 12)
                Text(ChainPane.heroTitle(transport: transport, reflector: reflector, relay: relay))
                    .font(.title2.bold())
            }
            Text(session.chainSummary)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }
}
