// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog
import SwiftUI

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "session")

/// Orchestrates the full "radio ↔ reflector ↔ audio" chain so the UI
/// doesn't have to. Specifically: **auto-starts the relay** whenever
/// the radio is in MMDVM mode AND a reflector session is live, and
/// auto-stops it when either precondition drops.
///
/// Users never toggle relay — they just connect a radio and pick a
/// reflector, and audio flows.
@Observable
@MainActor
public final class SessionCoordinator {
    public let transport: TransportCoordinator
    public let reflector: ReflectorCoordinator
    public let relay: RelayCoordinator

    private var preconditionsTask: Task<Void, Never>?

    public init(
        transport: TransportCoordinator,
        reflector: ReflectorCoordinator
    ) {
        self.transport = transport
        self.reflector = reflector
        self.relay = RelayCoordinator(transport: transport, reflector: reflector)
    }

    /// Start watching for precondition changes. Idempotent.
    public func activate() {
        guard preconditionsTask == nil else { return }
        // Poll the two coordinators at a low duty cycle and reconcile
        // the relay state. `@MainActor` isolation makes the reads
        // trivial; `reconcileRelay` is idempotent so there's no harm
        // in checking on every tick even when nothing changed.
        preconditionsTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.reconcileRelay()
                try? await Task.sleep(nanoseconds: 200_000_000)
            }
        }
    }

    /// Stop watching. Idempotent.
    public func deactivate() {
        preconditionsTask?.cancel()
        preconditionsTask = nil
    }

    /// `true` iff the radio is in MMDVM mode and the reflector session
    /// is live — regardless of the relay's own state. Used purely as
    /// the "should this be running?" input to reconcile.
    public var wantsRelay: Bool {
        transport.radioMode == .mmdvm && reflector.state == .connected
    }

    /// Convenience for UI: describes the chain in one sentence.
    public var chainSummary: String {
        let radioPart: String
        switch transport.state {
        case .disconnected:  radioPart = "No radio"
        case .connecting:    radioPart = "Radio connecting"
        case .failed:        radioPart = "Radio failed"
        case .connected:
            switch transport.radioMode {
            case .mmdvm:        radioPart = "TH-D75 · MMDVM"
            case .cat:          radioPart = "TH-D75 · CAT (not ready)"
            case .unknown:      radioPart = "TH-D75 · probing"
            case .unrecognized: radioPart = "TH-D75 · unknown mode"
            }
        }

        let refPart: String
        switch reflector.state {
        case .disconnected:   refPart = "no reflector"
        case .connecting:     refPart = "reflector connecting"
        case .failed:         refPart = "reflector failed"
        case .connected:
            if let r = reflector.connectedReflector {
                refPart = "\(r.name)\(reflector.reflectorModule)"
            } else {
                refPart = "linked"
            }
        }

        return "\(radioPart) ↔ \(refPart)"
    }

    // MARK: - Private

    /// Compare preconditions (`wantsRelay`) against the relay's
    /// current state and start/stop as needed. Idempotent — safe to
    /// call on every tick.
    private func reconcileRelay() async {
        let want = wantsRelay
        let running = relay.state == .running || relay.state == .starting
        if want && !running {
            log.info("SessionCoordinator: auto-starting relay")
            await relay.start()
        } else if !want && running {
            log.info("SessionCoordinator: auto-stopping relay (preconditions lost)")
            await relay.stop()
        }
    }
}
