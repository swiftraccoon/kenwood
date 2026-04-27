// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)
import SwiftUI
import AppKit

/// Contents of the system `MenuBarExtra` status item. Appears when the
/// user clicks the Lodestar icon in the system menu bar. Follows the
/// Apple HIG pattern for menu-bar extras: dense, glanceable, no
/// controls the app window doesn't also expose.
struct MenuBarExtraContent: View {
    let transport: TransportCoordinator
    let reflector: ReflectorCoordinator
    let session: SessionCoordinator?

    var body: some View {
        linkStatusHeader

        Divider()

        Button("Choose Reflector…") {
            activateApp()
            NotificationCenter.default.post(name: .lodestarShowReflectorPicker, object: nil)
        }
        .keyboardShortcut("l")

        Button("Connect Radio…") {
            activateApp()
            NotificationCenter.default.post(name: .lodestarShowDevicePicker, object: nil)
        }
        .keyboardShortcut("k")

        Divider()

        if reflector.state == .connected {
            Button("Disconnect Reflector") {
                Task { await reflector.disconnect() }
            }
        }
        if case .connected = transport.state {
            Button("Disconnect Radio") {
                Task { await transport.disconnect() }
            }
        }
        if reflector.state == .connected || transport.state == .connected {
            Divider()
        }

        Button("Show Lodestar Window") {
            activateApp()
        }
        .keyboardShortcut("0", modifiers: [.command])

        Divider()

        Button("Quit Lodestar") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
    }

    private var linkStatusHeader: some View {
        Text("\(radioText) ↔ \(reflectorText)")
            .font(.callout.monospaced())
    }

    private var radioText: String {
        switch transport.state {
        case .disconnected: return "No radio"
        case .connecting:   return "Radio connecting…"
        case .failed:       return "Radio failed"
        case .connected:    return transport.selectedDevice?.name ?? "TH-D75"
        }
    }

    private var reflectorText: String {
        switch reflector.state {
        case .disconnected: return "no reflector"
        case .connecting:   return "linking…"
        case .failed:       return "link failed"
        case .connected:
            if let r = reflector.connectedReflector {
                return "\(r.name)\(reflector.reflectorModule)"
            }
            return "linked"
        }
    }

    private func activateApp() {
        NSApp.activate(ignoringOtherApps: true)
        // Find the main window and bring to front. macOS 14+ gives
        // us access via NSApplication.mainWindow fallback.
        if let win = NSApp.windows.first(where: { $0.canBecomeKey }) {
            win.makeKeyAndOrderFront(nil)
        }
    }
}
#endif
