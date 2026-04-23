// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Top-level app routing. Two destinations only — one for the whole
/// radio ↔ reflector session, one for About. More sidebar clutter
/// would hurt clarity without adding function.
enum AppRoute: String, CaseIterable, Hashable, Identifiable {
    case session
    case about

    var id: String { rawValue }

    var title: String {
        switch self {
        case .session: return "Session"
        case .about:   return "About"
        }
    }

    var sfSymbol: String {
        switch self {
        case .session: return "dot.radiowaves.left.and.right"
        case .about:   return "info.circle"
        }
    }
}

struct LodestarShell: View {
    @State private var transport = TransportCoordinator()
    @State private var reflector = ReflectorCoordinator()
    @State private var session: SessionCoordinator? = nil
    @State private var route: AppRoute = .session

    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            #if os(macOS)
            macShell
            #else
            iosShell
            #endif
        }
        .onAppear {
            if session == nil {
                let s = SessionCoordinator(transport: transport, reflector: reflector)
                s.activate()
                session = s
            }
        }
        .onDisappear {
            session?.deactivate()
        }
        .onChange(of: scenePhase) { _, phase in
            // Graceful shutdown on background / inactive so the
            // reflector receives our unlink packet before the process
            // is suspended / terminated. Without this, reflectors
            // hold the stale session for 30–60 s and the next launch's
            // auto-connect gets rejected.
            if phase == .background || phase == .inactive {
                Task { @MainActor in
                    await session?.shutdown()
                }
            } else if phase == .active, session == nil {
                let s = SessionCoordinator(transport: transport, reflector: reflector)
                s.activate()
                session = s
            } else if phase == .active {
                // Returning from background: restart the watchers +
                // re-run auto-connect (shutdown cleared everything).
                session?.activate()
            }
        }
    }

    #if os(macOS)
    private var macShell: some View {
        NavigationSplitView {
            List(AppRoute.allCases, selection: $route) { r in
                NavigationLink(value: r) {
                    Label(r.title, systemImage: r.sfSymbol)
                }
            }
            .navigationTitle("Lodestar")
            .navigationSplitViewColumnWidth(min: 140, ideal: 180)
        } detail: {
            detailFor(route: route)
                .navigationTitle(route.title)
        }
    }
    #endif

    private var iosShell: some View {
        TabView(selection: $route) {
            ForEach(AppRoute.allCases) { r in
                NavigationStack {
                    detailFor(route: r)
                        .navigationTitle(r.title)
                }
                .tabItem {
                    Label(r.title, systemImage: r.sfSymbol)
                }
                .tag(r)
            }
        }
    }

    @ViewBuilder
    private func detailFor(route: AppRoute) -> some View {
        switch route {
        case .session:
            if let s = session {
                SessionScreen(session: s)
            } else {
                ProgressView()
            }
        case .about:
            AboutScreen()
        }
    }
}
