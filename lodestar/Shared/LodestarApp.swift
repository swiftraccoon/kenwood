// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// App entry point. On macOS we additionally register:
///   * a `Settings` scene (⌘, opens our tabbed preferences window),
///   * a `MenuBarExtra` status item (menu-bar link state + quick access),
///   * `AppCommands` that replace the default New-Item menu with
///     radio / reflector shortcuts and add a dedicated Reflector menu.
@main
@MainActor
struct LodestarApp: App {
    /// Coordinators live at the app level so that `MenuBarExtra` and
    /// `Settings` scenes share the same state as the main window.
    ///
    /// Eagerly constructed so every scene has a consistent state
    /// from the first frame — no placeholder swapping.
    @State private var transport: TransportCoordinator
    @State private var reflector: ReflectorCoordinator
    @State private var session: SessionCoordinator

    init() {
        // Install the Rust → os_log bridge BEFORE any FFI call so
        // the first tokio runtime spin-up + reflector connect events
        // are captured. Idempotent — tracing's global dispatcher
        // accepts the first installer and ignores later ones.
        installRustLogBridge()

        let transport = TransportCoordinator()
        let reflector = ReflectorCoordinator()
        let session = SessionCoordinator(transport: transport, reflector: reflector)
        session.activate()
        _transport = State(initialValue: transport)
        _reflector = State(initialValue: reflector)
        _session = State(initialValue: session)
    }

    var body: some Scene {
        #if os(macOS)
        mainWindow
            .defaultSize(width: 720, height: 760)
            .commands {
                AppCommands(transport: transport, reflector: reflector, session: session)
            }

        Settings {
            SettingsScene(transport: transport, reflector: reflector)
        }

        Window("Lodestar Log", id: "log") {
            LogViewerWindow()
        }
        .keyboardShortcut("l", modifiers: [.command, .shift])
        .defaultSize(width: 720, height: 480)

        MenuBarExtra {
            MenuBarExtraContent(
                transport: transport,
                reflector: reflector,
                session: session
            )
        } label: {
            Image(systemName: menuBarSymbol)
        }
        .menuBarExtraStyle(.menu)
        #else
        mainWindow
        #endif
    }

    private var mainWindow: some Scene {
        WindowGroup {
            ContentView()
                .environment(transport)
                .environment(reflector)
                .environment(session)
        }
    }

    /// The symbol used on the system menu bar reflects high-level
    /// link state so you can glance at it across the room.
    private var menuBarSymbol: String {
        if case .connected = reflector.state, case .connected = transport.state {
            return "dot.radiowaves.left.and.right"
        }
        if case .connected = reflector.state {
            return "antenna.radiowaves.left.and.right"
        }
        if case .connected = transport.state {
            return "antenna.radiowaves.left.and.right.slash"
        }
        return "dot.radiowaves.forward"
    }
}
