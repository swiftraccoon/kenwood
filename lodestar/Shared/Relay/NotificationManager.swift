// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog
import UserNotifications

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "notifications")

/// Thin wrapper around `UNUserNotificationCenter` for the passive
/// signals the app surfaces when its window isn't front-most:
/// reflector disconnect, radio BT drop, auto-reconnect outcomes.
///
/// Per Apple HIG: notifications should be infrequent, actionable,
/// and mirror something the user could already see in the window
/// they're not looking at. We do **not** notify on every heard
/// station — that would become spam for any active reflector.
@MainActor
public final class NotificationManager {
    public static let shared = NotificationManager()

    /// Tracks whether we've already asked for permission this session,
    /// so repeated authorization prompts don't stack up.
    private var askedThisSession = false

    private init() {}

    /// Request notification authorization if we haven't already.
    /// Safe to call repeatedly — the system coalesces and the
    /// `askedThisSession` flag short-circuits further prompts.
    public func requestAuthorizationIfNeeded() {
        guard !askedThisSession else { return }
        askedThisSession = true
        Task {
            do {
                let ok = try await UNUserNotificationCenter.current()
                    .requestAuthorization(options: [.alert, .sound])
                log.info("Notification auth granted: \(ok)")
            } catch {
                log.error("Notification auth failed: \(error)")
            }
        }
    }

    /// Reflector link dropped unexpectedly.
    public func reflectorDisconnected(reason: String) {
        post(
            id: "reflector-disconnected",
            title: "Reflector disconnected",
            body: reason,
            sound: .defaultCritical
        )
    }

    /// Auto-reconnect succeeded after a prior failure.
    public func reflectorReconnected(name: String) {
        post(
            id: "reflector-reconnected",
            title: "Reconnected to \(name)",
            body: "Voice relay resumed.",
            sound: nil
        )
    }

    /// Radio BT link dropped unexpectedly.
    public func radioDisconnected() {
        post(
            id: "radio-disconnected",
            title: "Radio disconnected",
            body: "Bluetooth link to the TH-D75 was lost.",
            sound: .defaultCritical
        )
    }

    /// Auto-connect on launch failed all retries.
    public func autoConnectFailed(what: String, reason: String) {
        post(
            id: "autoconnect-failed-\(what)",
            title: "\(what) auto-connect failed",
            body: reason,
            sound: nil
        )
    }

    // MARK: - Private

    private func post(
        id: String,
        title: String,
        body: String,
        sound: UNNotificationSound?
    ) {
        requestAuthorizationIfNeeded()

        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        if let sound {
            content.sound = sound
        }

        // Stable id per notification kind — re-posting replaces the
        // prior alert in Notification Center instead of stacking.
        let request = UNNotificationRequest(
            identifier: id,
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request) { err in
            if let err {
                log.error("Failed to post notification \(id): \(err)")
            }
        }
    }
}
