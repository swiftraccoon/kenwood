// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Top-level app destinations. Currently just the session view;
/// Map (future) and other routes would join here.
///
/// About is **not** a route — it lives in the macOS app menu via
/// `CommandGroup(replacing: .appInfo)` per Apple HIG.
enum AppRoute: String, CaseIterable, Hashable, Identifiable {
    case session

    var id: String { rawValue }

    var title: String {
        switch self {
        case .session: return "Session"
        }
    }

    var sfSymbol: String {
        switch self {
        case .session: return "dot.radiowaves.left.and.right"
        }
    }
}

struct LodestarShell: View {
    @Environment(TransportCoordinator.self) private var transport
    @Environment(ReflectorCoordinator.self) private var reflector
    @Environment(SessionCoordinator.self) private var session
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
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                // A linked app stays alive in the background: the
                // running audio pipeline (UIBackgroundModes: audio)
                // holds the process open — and with it the reflector
                // UDP session and the USB user client, so the relay
                // keeps relaying with the screen off. Only an idle
                // app (no reflector link) shuts down and suspends
                // normally; without the graceful unlink, reflectors
                // hold the stale session for 30–60 s and the next
                // launch's auto-connect gets rejected.
                if reflector.state != .connected {
                    Task { @MainActor in
                        #if os(iOS)
                        // USB user-client connections don't survive
                        // app suspension — tear down first so the
                        // dext isn't left holding a doorbell for a
                        // frozen process.
                        await transport.handleScenePhaseBackground()
                        #endif
                        await session.shutdown()
                    }
                }
            case .active:
                // Both calls self-guard: activate() is idempotent,
                // and the transport only reconnects if backgrounding
                // actually tore a live link down.
                session.activate()
                #if os(iOS)
                Task { @MainActor in
                    // Rescan (radio may have been plugged in while
                    // suspended) and restore the pre-background link.
                    await transport.handleScenePhaseActive()
                }
                #endif
            default:
                // .inactive is transient — Notification Center pulls,
                // incoming-call UI, the app switcher. Tearing down
                // here killed live sessions on trivial interruptions.
                break
            }
        }
    }

    #if os(macOS)
    private var macShell: some View {
        // Single destination — skip the NavigationSplitView.
        // If future routes land, reintroduce a sidebar here.
        NavigationStack {
            SessionScreen(session: session)
                .navigationTitle("Lodestar")
        }
    }
    #endif

    private var iosShell: some View {
        NavigationStack {
            SessionScreen(session: session)
                .navigationTitle("Lodestar")
        }
    }
}
