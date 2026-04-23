// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation

/// UI-facing state store for the active D-STAR reflector session.
///
/// Implements the Rust-side `ReflectorObserver` callback protocol and
/// marshals each event onto the main actor where SwiftUI views can
/// observe it via `@Observable`. Owns the `ReflectorSession` handle so
/// the coordinator outlives any individual view and survives tab
/// switches without tearing the session down.
@Observable
@MainActor
public final class ReflectorCoordinator: ReflectorObserver {
    // MARK: - Persisted settings

    /// Operator callsign. Persisted via `UserDefaults`.
    public var callsign: String {
        didSet { UserDefaults.standard.set(callsign, forKey: Self.callsignKey) }
    }

    /// Local module letter presented to the reflector as our source module.
    public var localModule: String {
        didSet { UserDefaults.standard.set(localModule, forKey: Self.localModuleKey) }
    }

    /// Target module letter on the reflector (e.g. `C` on REF030C).
    public var reflectorModule: String {
        didSet { UserDefaults.standard.set(reflectorModule, forKey: Self.reflectorModuleKey) }
    }

    /// When `true`, `tryAutoConnect()` will reconnect on launch to the
    /// last-used reflector. Persisted.
    public var autoConnectReflector: Bool {
        didSet { UserDefaults.standard.set(autoConnectReflector, forKey: Self.autoConnectKey) }
    }

    /// Name (e.g. `REF030`) of the most recently connected reflector.
    /// Captured on every successful link-up; persisted so
    /// `tryAutoConnect()` can find it on the next launch.
    public private(set) var rememberedReflectorName: String? {
        didSet { UserDefaults.standard.set(rememberedReflectorName, forKey: Self.rememberedNameKey) }
    }

    // MARK: - Runtime state

    public private(set) var state: State = .disconnected
    public private(set) var currentStream: StreamSnapshot?
    public private(set) var recentlyHeard: [HeardEntry] = []
    public private(set) var lastError: String?
    public private(set) var isBusy: Bool = false
    public private(set) var connectedReflector: Reflector?

    /// Every event applied to the coordinator is also forwarded to
    /// this hook (after state update). `RelayCoordinator` sets this
    /// to wire reflector→radio voice frames.
    public var relayHook: (@MainActor (ReflectorEvent) -> Void)?

    /// The live session handle. Exposed so `RelayCoordinator` can call
    /// `sendHeader`/`sendVoice`/`sendEot` for the radio→reflector path.
    public var activeSession: ReflectorSession? { session }

    // MARK: - Private

    private var session: ReflectorSession?

    private static let callsignKey = "lodestar.callsign"
    private static let localModuleKey = "lodestar.localModule"
    private static let reflectorModuleKey = "lodestar.reflectorModule"
    private static let autoConnectKey = "lodestar.autoConnectReflector"
    private static let rememberedNameKey = "lodestar.rememberedReflectorName"

    // MARK: - Init

    public init() {
        let defaults = UserDefaults.standard
        self.callsign = defaults.string(forKey: Self.callsignKey) ?? ""
        self.localModule = defaults.string(forKey: Self.localModuleKey) ?? "C"
        self.reflectorModule = defaults.string(forKey: Self.reflectorModuleKey) ?? "C"
        self.autoConnectReflector = defaults.bool(forKey: Self.autoConnectKey)
        self.rememberedReflectorName = defaults.string(forKey: Self.rememberedNameKey)
    }

    // MARK: - Actions

    public func connect(to reflector: Reflector) async {
        guard !callsign.isEmpty else {
            state = .failed("Enter your operator callsign first.")
            lastError = state.errorMessage
            return
        }

        // If already connected (or mid-connect) to something, tear that
        // session down first. This is the "switch reflector" path — the
        // picker sheet's only call site. Without this, picking a new
        // reflector while connected silently no-ops.
        if session != nil {
            await disconnect()
        }

        isBusy = true
        defer { isBusy = false }
        state = .connecting
        lastError = nil
        connectedReflector = reflector

        do {
            let observer: any ReflectorObserver = self
            let s = try await connectReflector(
                callsign: callsign,
                reflector: reflector,
                localModule: localModule,
                reflectorModule: reflectorModule,
                observer: observer
            )
            session = s
            state = .connected
            // Remember this reflector so `tryAutoConnect()` can find it
            // on the next launch. Captured unconditionally; the user's
            // `autoConnectReflector` toggle controls whether we act on it.
            rememberedReflectorName = reflector.name
        } catch {
            let msg = String(describing: error)
            state = .failed(msg)
            lastError = msg
            connectedReflector = nil
        }
    }

    /// Auto-reconnect to the remembered reflector on launch, if enabled
    /// and the remembered name is present in the bundled directory.
    /// Idempotent and silent when conditions aren't met.
    ///
    /// Retries with backoff on rejection — if the app was recently
    /// terminated without a graceful `disconnect()`, reflectors hold
    /// the previous UDP session in memory for 30–60 s until keepalive
    /// timeout and reject fresh LINK attempts during that window.
    /// The backoff schedule gives the reflector time to clear us.
    public func tryAutoConnect() async {
        guard autoConnectReflector, session == nil else { return }
        guard case .disconnected = state else { return }
        guard let name = rememberedReflectorName else { return }
        guard !callsign.isEmpty else {
            // No callsign yet; can't connect. The user will configure
            // one the first time they open the picker.
            return
        }
        let target = defaultReflectors().first(where: { $0.name == name })
        guard let r = target else {
            lastError = "Auto-connect: reflector \(name) not in the bundled directory"
            return
        }

        // Delays chosen so the total backoff window (~30 s) exceeds
        // the typical reflector keepalive-timeout window for stale
        // sessions. `0` = try immediately on the first attempt.
        let delaysNs: [UInt64] = [0, 3_000_000_000, 10_000_000_000, 20_000_000_000]
        for (attempt, delay) in delaysNs.enumerated() {
            if delay > 0 {
                try? await Task.sleep(nanoseconds: delay)
            }
            // Between retries, re-check: user might have manually
            // connected, toggled the setting off, or disconnected.
            // We retry when the previous attempt ended in `.failed`
            // or we're still at `.disconnected` — but NOT while a
            // `.connecting` / `.connected` session is in flight or live.
            guard autoConnectReflector, session == nil else { return }
            switch state {
            case .connecting, .connected:
                return
            case .disconnected, .failed:
                // If the previous attempt failed, clear the error so
                // the next connect() starts from a clean slate.
                if case .failed = state {
                    state = .disconnected
                    lastError = nil
                }
            }

            await connect(to: r)
            if case .connected = state {
                if attempt > 0 {
                    // Clear whatever transient error the earlier
                    // attempt surfaced — the retry succeeded.
                    lastError = nil
                }
                return
            }
        }
        // All attempts failed; whatever `connect(to:)` left in
        // `state` / `lastError` is the final answer.
    }

    public func disconnect() async {
        guard let s = session else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            try await s.disconnect()
        } catch {
            lastError = "disconnect: \(error)"
        }
        session = nil
        connectedReflector = nil
        currentStream = nil
        state = .disconnected
    }

    public func clearHeardHistory() {
        recentlyHeard.removeAll()
    }

    /// Insert a recently-heard entry for a transmission that originated
    /// on *our* radio and was relayed to the reflector. Reflectors
    /// typically don't echo the sender's own stream back, so without
    /// this the operator's own transmissions never show up in the
    /// local history even though they're visible on the reflector's
    /// last-heard page.
    public func logLocalTransmission(
        mycall: String,
        suffix: String,
        urcall: String,
        startedAt: Date,
        frames: UInt32,
        text: String?
    ) {
        let entry = HeardEntry(
            mycall: mycall,
            suffix: suffix,
            urcall: urcall,
            endedAt: Date(),
            duration: Date().timeIntervalSince(startedAt),
            frames: frames,
            endReason: "local TX",
            text: text,
            position: nil
        )
        recentlyHeard.insert(entry, at: 0)
        if recentlyHeard.count > 100 {
            recentlyHeard.removeLast()
        }
    }

    // MARK: - ReflectorObserver (callback from tokio background task)

    public nonisolated func onEvent(event: ReflectorEvent) {
        // Hop onto the main actor so `@Observable` change tracking
        // fires on the same actor SwiftUI is rendering on.
        Task { @MainActor [weak self] in
            self?.apply(event)
        }
    }

    // MARK: - Event application

    private func apply(_ event: ReflectorEvent) {
        defer {
            // Forward to the relay hook after state is updated. If the
            // hook throws or blocks, subsequent events wait; keep it fast.
            relayHook?(event)
        }
        switch event {
        case .connected:
            state = .connected

        case .disconnected(let reason):
            state = .disconnected
            lastError = "reflector disconnected: \(reason)"
            finalizeCurrentStream(reason: reason)

        case .pollEcho:
            break

        case .voiceStart(let streamId, let mycall, let suffix, let urcall, let rpt1, let rpt2, _):
            // `_` = headerBytes. ReflectorCoordinator shows metadata
            // only; `RelayCoordinator` consumes the raw header to
            // forward to the radio as an MMDVM DStarHeader frame.
            currentStream = StreamSnapshot(
                id: streamId,
                mycall: mycall,
                suffix: suffix,
                urcall: urcall,
                rpt1: rpt1,
                rpt2: rpt2,
                framesReceived: 0,
                startedAt: Date(),
                latestText: nil,
                latestPosition: nil
            )

        case .voiceFrame(let streamId, _, _):
            if var s = currentStream, s.id == streamId {
                s.framesReceived &+= 1
                currentStream = s
            }

        case .slowDataUpdate(let streamId, let text, let position):
            if var s = currentStream, s.id == streamId {
                s.latestText = text
                s.latestPosition = position
                currentStream = s
            }

        case .voiceEnd(let streamId, let reason, let text, let position):
            if let s = currentStream, s.id == streamId {
                // Prefer the authoritative values carried on VoiceEnd;
                // fall back to whatever the snapshot last saw.
                appendHeard(
                    from: s,
                    endReason: reason,
                    text: text ?? s.latestText,
                    position: position ?? s.latestPosition
                )
            }
            currentStream = nil

        case .ended:
            state = .disconnected
            session = nil
            connectedReflector = nil
        }
    }

    private func finalizeCurrentStream(reason: String) {
        if let s = currentStream {
            appendHeard(
                from: s,
                endReason: "interrupted: \(reason)",
                text: s.latestText,
                position: s.latestPosition
            )
        }
        currentStream = nil
    }

    private func appendHeard(
        from s: StreamSnapshot,
        endReason: String,
        text: String?,
        position: GpsPosition?
    ) {
        let entry = HeardEntry(
            mycall: s.mycall,
            suffix: s.suffix,
            urcall: s.urcall,
            endedAt: Date(),
            duration: Date().timeIntervalSince(s.startedAt),
            frames: s.framesReceived,
            endReason: endReason,
            text: text,
            position: position
        )
        recentlyHeard.insert(entry, at: 0)
        // Cap so the list doesn't grow unbounded in long sessions.
        if recentlyHeard.count > 100 {
            recentlyHeard.removeLast()
        }
    }

    // MARK: - Nested types

    public enum State: Equatable {
        case disconnected
        case connecting
        case connected
        case failed(String)

        var errorMessage: String? {
            if case .failed(let m) = self { return m }
            return nil
        }
    }

    public struct StreamSnapshot: Identifiable, Equatable, Sendable {
        public let id: UInt16
        public let mycall: String
        public let suffix: String
        public let urcall: String
        public let rpt1: String
        public let rpt2: String
        public var framesReceived: UInt32
        public let startedAt: Date
        /// Latest assembled 20-byte TX message for this stream, if any.
        /// Kept in sync with `SlowDataUpdate` events.
        public var latestText: String?
        /// Latest parsed DPRS position for this stream, if any.
        public var latestPosition: GpsPosition?
    }

    public struct HeardEntry: Identifiable, Equatable, Sendable {
        public let id: UUID = UUID()
        public let mycall: String
        public let suffix: String
        public let urcall: String
        public let endedAt: Date
        public let duration: TimeInterval
        public let frames: UInt32
        public let endReason: String
        /// Slow-data text message ("TX message" set on the operator's
        /// radio), if a complete 20-byte message was assembled during
        /// the stream.
        public let text: String?
        /// Final DPRS position reported during the stream, if any.
        public let position: GpsPosition?
    }
}
