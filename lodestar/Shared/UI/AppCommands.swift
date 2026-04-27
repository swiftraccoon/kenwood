// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// App-level keyboard shortcuts, exposed as `.commands { AppCommands(...) }`
/// on the main window group. Follows the standard macOS menu layout:
/// File, Radio, Reflector, View — with destructive actions at the bottom
/// of each menu and confirmed with ⌘⇧-destructive pattern where needed.
///
/// Uses `NotificationCenter` to signal the `SessionScreen` instead of
/// direct method calls, because `Commands` builders can't hold `@State`
/// or call actor methods directly — they live outside the view tree.
struct AppCommands: Commands {
    let transport: TransportCoordinator
    let reflector: ReflectorCoordinator
    let session: SessionCoordinator

    var body: some Commands {
        // Customize the standard "About Lodestar…" entry (auto-generated
        // by AppKit at the top of the app menu) so it shows our credits
        // + version alongside the auto-populated bundle info.
        CommandGroup(replacing: .appInfo) {
            Button("About Lodestar") {
                showAboutPanel()
            }
        }

        // Replace the default "New Item" entry with a Radio group — no
        // document-new concept applies to this app.
        CommandGroup(replacing: .newItem) {
            Button("Connect Radio…") {
                NotificationCenter.default.post(name: .lodestarShowDevicePicker, object: nil)
            }
            .keyboardShortcut("k", modifiers: [.command])

            Button("Re-probe Radio Mode") {
                Task { await transport.probeRadioMode() }
            }
            .keyboardShortcut("r", modifiers: [.command, .option])
            .disabled(!transport.isConnected)

            Divider()

            Button("Disconnect Radio") {
                Task { await transport.disconnect() }
            }
            .keyboardShortcut("d", modifiers: [.command, .shift])
            .disabled(!transport.isConnected)
        }

        // Dedicated Reflector menu between File and Edit.
        CommandMenu("Reflector") {
            Button("Choose Reflector…") {
                NotificationCenter.default.post(name: .lodestarShowReflectorPicker, object: nil)
            }
            .keyboardShortcut("l", modifiers: [.command])

            Button("Disconnect Reflector") {
                Task { await reflector.disconnect() }
            }
            .keyboardShortcut("d", modifiers: [.command])
            .disabled(reflector.state != .connected)

            Divider()

            Button("Show Recently Heard…") {
                NotificationCenter.default.post(name: .lodestarShowHeardHistory, object: nil)
            }
            .keyboardShortcut("h", modifiers: [.command, .shift])
            .disabled(reflector.recentlyHeard.isEmpty)

            Button("Clear Recently Heard") {
                reflector.clearHeardHistory()
            }
            .keyboardShortcut(.delete, modifiers: [.command])
            .disabled(reflector.recentlyHeard.isEmpty)
        }

        // Replace default Help so we can link to the repo — still
        // in the standard trailing position on macOS.
        CommandGroup(replacing: .help) {
            Link(destination: URL(string: "https://github.com/swiftraccoon/kenwood")!) {
                Text("Lodestar on GitHub")
            }
        }
    }
}

extension Notification.Name {
    /// Triggered by `File → Connect Radio…` (⌘K).
    static let lodestarShowDevicePicker = Notification.Name("lodestar.showDevicePicker")
    /// Triggered by `Reflector → Choose Reflector…` (⌘L).
    static let lodestarShowReflectorPicker = Notification.Name("lodestar.showReflectorPicker")
    /// Triggered by `Reflector → Show Recently Heard…` (⌘⇧H).
    static let lodestarShowHeardHistory = Notification.Name("lodestar.showHeardHistory")
}

private extension TransportCoordinator {
    var isConnected: Bool {
        if case .connected = state { return true }
        return false
    }
}

#if os(macOS)
import AppKit

/// Render the standard NSApplication about panel with our credits.
/// Matches the exact presentation style used by every Apple-shipped
/// app — bundle icon, app name, short version, copyright, and a
/// "Credits" roll that opens as a secondary sheet.
@MainActor
private func showAboutPanel() {
    let credits = NSAttributedString(
        string: """
        D-STAR gateway for the Kenwood TH-D75.

        Reflector hosts list: ircDDBGateway (G4KLX).
        D-STAR protocol codec: dstar-gateway-core.
        MMDVM protocol: MMDVMHost (G4KLX) via mmdvm-core.
        AMBE decoder: mbelib-rs.
        """,
        attributes: [
            .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        ]
    )
    NSApp.orderFrontStandardAboutPanel(options: [
        NSApplication.AboutPanelOptionKey.credits: credits,
        NSApplication.AboutPanelOptionKey(rawValue: "Copyright"):
            "© 2026 Swift Raccoon. GPL-2.0-or-later / GPL-3.0-or-later.",
    ])
    NSApp.activate(ignoringOtherApps: true)
}
#else
@MainActor
private func showAboutPanel() { /* no-op on iOS; About lives in Settings */ }
#endif
