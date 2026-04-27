// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)
import SwiftUI
import OSLog
import AppKit

/// Read-only window showing recent Unified Log entries for the app's
/// subsystems. Opened from `View → Show Log` (⌘⇧L). Uses Apple's
/// public `OSLogStore` API — works on any dev-signed or notarized
/// macOS build without special entitlements.
public struct LogViewerWindow: View {
    @State private var entries: [LogRow] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var selectedSubsystem: String = "all"

    /// How far back to scan on each reload, in seconds. Shorter windows
    /// return results in under a second; longer windows (5+ minutes)
    /// can take multiple seconds because `OSLogStore.getEntries` walks
    /// the shared-cache store synchronously.
    @State private var scanWindow: TimeInterval = 60

    private let swiftSubsystem = "org.swiftraccoon.lodestar"
    private let rustSubsystem = "org.swiftraccoon.lodestar.rust"

    private var subsystems: [(label: String, value: String)] {
        [
            ("All", "all"),
            ("Rust", "\(rustSubsystem):"),
            ("Transport", "\(swiftSubsystem):transport"),
            ("Reflector", "\(swiftSubsystem):reflector"),
            ("Relay", "\(swiftSubsystem):relay"),
            ("Session", "\(swiftSubsystem):session"),
            ("Notifications", "\(swiftSubsystem):notifications"),
        ]
    }

    public init() {}

    public var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            content
        }
        .frame(minWidth: 640, minHeight: 420)
        .task {
            // `.task` runs at first-appear but on a detached priority
            // — `.onAppear { Task { ... } }` previously scheduled the
            // async work on MainActor which blocked the window from
            // painting until the OSLogStore scan returned.
            await reload()
        }
    }

    private var toolbar: some View {
        HStack(spacing: 10) {
            Picker("Filter", selection: $selectedSubsystem) {
                ForEach(subsystems, id: \.value) {
                    Text($0.label).tag($0.value)
                }
            }
            .pickerStyle(.segmented)
            .onChange(of: selectedSubsystem) { _, _ in Task { await reload() } }

            Picker("Window", selection: $scanWindow) {
                Text("1 m").tag(TimeInterval(60))
                Text("5 m").tag(TimeInterval(300))
                Text("15 m").tag(TimeInterval(900))
            }
            .pickerStyle(.segmented)
            .frame(width: 140)
            .onChange(of: scanWindow) { _, _ in Task { await reload() } }

            Spacer()

            if isLoading {
                ProgressView().controlSize(.small)
            }

            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(joinedText, forType: .string)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            .disabled(entries.isEmpty)

            Button {
                Task { await reload() }
            } label: {
                Label("Reload", systemImage: "arrow.clockwise")
            }
            .disabled(isLoading)
        }
        .padding(10)
    }

    @ViewBuilder
    private var content: some View {
        if let errorMessage {
            ContentUnavailableView(
                "Log unavailable",
                systemImage: "exclamationmark.triangle",
                description: Text(errorMessage)
            )
        } else if entries.isEmpty {
            ContentUnavailableView(
                "No log entries",
                systemImage: "doc.text",
                description: Text("Nothing has been logged under this subsystem in the last 5 minutes.")
            )
        } else {
            // One single monospaced text block inside a ScrollView.
            // `.textSelection(.enabled)` + a single `Text` lets the
            // user drag-select across rows, copy multi-line spans,
            // and ⌘A select-all — behaviour you expect from any log
            // viewer, which per-row `Text` + `.textSelection` does not
            // give you.
            ScrollView {
                Text(joinedText)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
        }
    }

    /// Full log flattened into a single selectable/copyable string.
    private var joinedText: String {
        entries
            .map { row in
                let ts = timestampFormatter.string(from: row.timestamp)
                let marker = marker(for: row.level)
                return "\(ts) \(marker) [\(row.category)] \(row.message)"
            }
            .joined(separator: "\n")
    }

    private var timestampFormatter: DateFormatter {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }

    private func marker(for level: OSLogEntryLog.Level) -> String {
        switch level {
        case .error: return "⚠︎"
        case .fault: return "✗"
        default:     return " "
        }
    }

    private func reload() async {
        isLoading = true
        defer { isLoading = false }

        // `OSLogStore.getEntries` is synchronous and can take several
        // seconds to scan the shared-cache store — run it OFF the
        // MainActor so the window paints its chrome immediately and
        // the ProgressView spinner is actually visible.
        let filter = selectedSubsystem
        let swiftSub = swiftSubsystem
        let rustSub = rustSubsystem
        let window = scanWindow

        struct Outcome: Sendable {
            let rows: [LogRow]
            let error: String?
        }

        let outcome: Outcome = await Task.detached(priority: .userInitiated) {
            do {
                let store = try OSLogStore(scope: .currentProcessIdentifier)
                let since = store.position(date: Date().addingTimeInterval(-window))
                let predicate = NSPredicate(
                    format: "subsystem BEGINSWITH %@ OR subsystem BEGINSWITH %@",
                    swiftSub, rustSub
                )
                let all = try store.getEntries(at: since, matching: predicate)

                let rows: [LogRow] = all.compactMap { entry -> LogRow? in
                    guard let log = entry as? OSLogEntryLog else { return nil }
                    if filter != "all" {
                        let actual = "\(log.subsystem):\(log.category)"
                        let subsystemOnly = "\(log.subsystem):"
                        if filter.hasSuffix(":") {
                            if subsystemOnly != filter { return nil }
                        } else if filter != actual {
                            return nil
                        }
                    }
                    return LogRow(
                        id: UUID(),
                        timestamp: log.date,
                        category: log.category,
                        message: log.composedMessage,
                        level: log.level
                    )
                }
                return Outcome(rows: rows, error: nil)
            } catch {
                return Outcome(rows: [], error: String(describing: error))
            }
        }.value

        entries = outcome.rows
        errorMessage = outcome.error
    }
}

private struct LogRow: Identifiable {
    let id: UUID
    let timestamp: Date
    let category: String
    let message: String
    let level: OSLogEntryLog.Level
}
#endif
