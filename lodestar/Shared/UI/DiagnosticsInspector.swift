// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Radio-link diagnostics. `.card` is the classic inline dashboard
/// card (narrow layout); `.panel` is the wide layout's toolbar-toggled
/// floating inspector over the map. Content and actions are identical:
/// probe/identify plus a one-tap transport + dext-event-ring dump —
/// no Console.app required. Diagnostics actions stay available even
/// when `isBusy` gates the transport-writing buttons; a stuck busy
/// flow is exactly when they're needed most.
struct DiagnosticsInspector: View {
    enum Presentation {
        case card
        case panel
    }

    let transport: TransportCoordinator
    let presentation: Presentation

    @State private var diagnosticsDump: String?

    var body: some View {
        switch presentation {
        case .card:
            if transport.state == .connected || transport.lastProbeErrorText != nil {
                GroupBox { content }
            }
        case .panel:
            content
                .padding()
                .frame(width: 360, alignment: .leading)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Radio diagnostics")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            if !transport.lastResponseText.isEmpty {
                Text(transport.lastResponseText)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
            }
            if let diag = diagnosticsDump {
                ScrollView {
                    Text(diag)
                        .font(.caption2.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: presentation == .panel ? 420 : 220)
            }
            HStack(spacing: 12) {
                // Only the transport-writing actions respect
                // isBusy. Diagnostics stay available ALWAYS —
                // a stuck busy flow is exactly when they're
                // needed most.
                Group {
                    Button("Send ID") {
                        Task { await transport.sendIdentify() }
                    }
                    Button("Re-probe") {
                        Task { await transport.probeRadioMode() }
                    }
                }
                .disabled(transport.isBusy)
                Button(diagnosticsDump == nil ? "Show diagnostics" : "Refresh") {
                    Task { diagnosticsDump = await transport.diagnosticsText() }
                }
                if let diag = diagnosticsDump {
                    Button("Copy") {
                        #if os(iOS)
                        UIPasteboard.general.string = diag
                        #else
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(diag, forType: .string)
                        #endif
                    }
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
