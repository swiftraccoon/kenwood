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
            // Graceful shutdown on background / inactive so the
            // reflector receives our unlink packet before the process
            // is suspended / terminated. Without this, reflectors
            // hold the stale session for 30–60 s and the next launch's
            // auto-connect gets rejected.
            if phase == .background || phase == .inactive {
                Task { @MainActor in
                    await session.shutdown()
                }
            } else if phase == .active {
                // Returning from background: restart the watchers +
                // re-run auto-connect (shutdown cleared everything).
                session.activate()
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
