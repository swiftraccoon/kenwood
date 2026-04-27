// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

/// Forwards Rust `tracing` events from `lodestar-core` into Apple's
/// Unified Log under subsystem `org.swiftraccoon.lodestar.rust`, with
/// the event `target` as the category. That puts Rust-side diagnostics
/// alongside Swift-side `Logger` calls in `Console.app` /
/// `OSLogStore` / our in-app Log Viewer.
///
/// Implements the UniFFI callback trait [`LogSink`] exported from
/// `lodestar-core/src/lib.rs`. Must be `@unchecked Sendable` because
/// UniFFI requires the trait impl to be `Send + Sync`, but our
/// `cachedLoggers` dictionary mutation already serialises on the
/// concurrent queue.
final class RustLogBridge: LogSink, @unchecked Sendable {
    static let shared = RustLogBridge()

    private static let subsystem = "org.swiftraccoon.lodestar.rust"

    /// Cache one `Logger` per category so we don't pay the
    /// `os_log_create` cost on every event. Accessed from whichever
    /// tokio thread fires the event; protected by the concurrent
    /// queue's barrier pattern.
    private var cachedLoggers: [String: Logger] = [:]
    private let queue = DispatchQueue(
        label: "org.swiftraccoon.lodestar.rust-log",
        attributes: .concurrent
    )

    private init() {}

    func log(level: LogLevel, target: String, message: String) {
        let logger = loggerFor(category: target)
        switch level {
        case .trace, .debug: logger.debug("\(message, privacy: .public)")
        case .info:          logger.info("\(message, privacy: .public)")
        case .warn:          logger.warning("\(message, privacy: .public)")
        case .error:         logger.error("\(message, privacy: .public)")
        }
    }

    private func loggerFor(category: String) -> Logger {
        // Fast path: read cached logger.
        if let cached = queue.sync(execute: { cachedLoggers[category] }) {
            return cached
        }
        // Slow path: create + cache.
        let logger = Logger(subsystem: Self.subsystem, category: category)
        queue.async(flags: .barrier) { [weak self] in
            self?.cachedLoggers[category] = logger
        }
        return logger
    }
}

/// Idempotent: install the Rust tracing → os_log bridge. Call once
/// at app launch.
@MainActor
func installRustLogBridge() {
    initTracing(sink: RustLogBridge.shared)
}
