// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "reflector")

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

    /// User's starred reflectors, persisted. Surfaced ahead of the
    /// curated "Featured" list in the picker.
    public private(set) var favoriteReflectorNames: [String] {
        didSet { UserDefaults.standard.set(favoriteReflectorNames, forKey: Self.favoritesKey) }
    }

    /// When `true`, the recently-heard list is saved to
    /// `UserDefaults` on every mutation and restored on launch so
    /// the history survives quits and reboots.
    public var persistRecentlyHeard: Bool {
        didSet {
            UserDefaults.standard.set(persistRecentlyHeard, forKey: Self.persistHeardKey)
            if persistRecentlyHeard {
                saveHeardHistory()
            } else {
                UserDefaults.standard.removeObject(forKey: Self.heardHistoryKey)
            }
        }
    }

    /// Cap on how many heard rows render inline on the dashboard.
    /// Overflow lives behind the "Show all" sheet. Persisted.
    public var inlineHeardLimit: Int {
        didSet { UserDefaults.standard.set(inlineHeardLimit, forKey: Self.inlineHeardLimitKey) }
    }

    public func toggleFavorite(name: String) {
        if let idx = favoriteReflectorNames.firstIndex(of: name) {
            favoriteReflectorNames.remove(at: idx)
        } else {
            favoriteReflectorNames.append(name)
        }
    }

    public func isFavorite(_ name: String) -> Bool {
        favoriteReflectorNames.contains(name)
    }

    /// When `true`, reflector voice plays through this device's
    /// speakers (decoded on-device). Persisted. Automatically
    /// suppressed while the radio relay is running so audio doesn't
    /// play twice.
    public var monitorAudioEnabled: Bool {
        didSet {
            UserDefaults.standard.set(monitorAudioEnabled, forKey: Self.monitorAudioKey)
            updateMonitorSuspension()
        }
    }

    /// Set by the relay while it owns the audio path (radio speaker).
    public var suppressMonitorForRelay: Bool = false {
        didSet { updateMonitorSuspension() }
    }

    /// Loss statistics for the most recently ended stream, if any.
    public private(set) var lastStreamStats: RxStreamStats?

    /// On-device playback engine for decoded reflector voice.
    public let audioPlayer = ReflectorAudioPlayer()
    private let rxPipeline = RxAudioPipeline()

    private func updateMonitorSuspension() {
        audioPlayer.isSuspended = !monitorAudioEnabled || suppressMonitorForRelay
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

    /// Serial event mailbox. The Rust callback thread yields into the
    /// continuation; ONE main-actor pump task applies events in
    /// arrival order. Independent `Task {}` spawns per event would
    /// race each other and could apply voiceFrame before voiceStart.
    private let eventContinuation: AsyncStream<ReflectorEvent>.Continuation
    private var eventPump: Task<Void, Never>?

    /// `true` when the user just tapped "Disconnect reflector" — so
    /// the subsequent `.disconnected` event is expected and the
    /// auto-reconnect scheduler should NOT fire. Cleared once the
    /// event is applied.
    private var userInitiatedDisconnect: Bool = false

    /// Scheduled auto-reconnect task, held so we can cancel it on
    /// manual user actions (e.g. picking a different reflector).
    private var reconnectTask: Task<Void, Never>?

    private static let callsignKey = "lodestar.callsign"
    private static let localModuleKey = "lodestar.localModule"
    private static let reflectorModuleKey = "lodestar.reflectorModule"
    private static let autoConnectKey = "lodestar.autoConnectReflector"
    private static let rememberedNameKey = "lodestar.rememberedReflectorName"
    private static let favoritesKey = "lodestar.favoriteReflectors"
    private static let persistHeardKey = "lodestar.persistRecentlyHeard"
    private static let heardHistoryKey = "lodestar.recentlyHeardArchive"
    private static let inlineHeardLimitKey = "lodestar.inlineHeardLimit"
    private static let monitorAudioKey = "lodestar.monitorAudio"

    // MARK: - Init

    public init() {
        var continuation: AsyncStream<ReflectorEvent>.Continuation!
        let stream = AsyncStream<ReflectorEvent>(bufferingPolicy: .unbounded) { c in
            continuation = c
        }
        self.eventContinuation = continuation

        let defaults = UserDefaults.standard
        self.callsign = defaults.string(forKey: Self.callsignKey) ?? ""
        self.localModule = defaults.string(forKey: Self.localModuleKey) ?? "C"
        self.reflectorModule = defaults.string(forKey: Self.reflectorModuleKey) ?? "C"
        self.autoConnectReflector = defaults.bool(forKey: Self.autoConnectKey)
        self.rememberedReflectorName = defaults.string(forKey: Self.rememberedNameKey)
        self.favoriteReflectorNames = defaults.stringArray(forKey: Self.favoritesKey) ?? []
        self.persistRecentlyHeard = defaults.bool(forKey: Self.persistHeardKey)
        // Default inline limit = 5; clamp 1…50 on load in case an old
        // value got out of range.
        let storedLimit = defaults.integer(forKey: Self.inlineHeardLimitKey)
        self.inlineHeardLimit = max(1, min(50, storedLimit == 0 ? 5 : storedLimit))
        self.monitorAudioEnabled = defaults.object(forKey: Self.monitorAudioKey) as? Bool ?? true
        if persistRecentlyHeard {
            self.recentlyHeard = Self.loadHeardHistory()
        }

        eventPump = Task { @MainActor [weak self] in
            for await event in stream {
                guard let self else { return }
                self.apply(event)
            }
        }

        updateMonitorSuspension()
    }

    deinit {
        // The pump task holds the stream strongly, so dropping the
        // continuation alone never terminates it — finish explicitly
        // or the parked pump task leaks per coordinator instance.
        eventContinuation.finish()
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
            // A live link starts the audio render pipeline even before
            // any voice arrives — background execution rides on it.
            audioPlayer.beginKeepAlive()
        } catch {
            let msg = error.displayMessage
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
        if case .failed(let message) = state {
            NotificationManager.shared.autoConnectFailed(what: "Reflector", reason: message)
        }
    }

    public func disconnect() async {
        guard let s = session else { return }
        userInitiatedDisconnect = true
        reconnectTask?.cancel()
        reconnectTask = nil
        isBusy = true
        defer { isBusy = false }
        do {
            try await s.disconnect()
        } catch {
            lastError = "disconnect: \(error.displayMessage)"
        }
        session = nil
        connectedReflector = nil
        currentStream = nil
        state = .disconnected
        // User chose to unlink — release the audio keep-alive so the
        // app can suspend normally in the background.
        audioPlayer.endKeepAlive()
    }

    public func clearHeardHistory() {
        recentlyHeard.removeAll()
        if persistRecentlyHeard {
            UserDefaults.standard.removeObject(forKey: Self.heardHistoryKey)
        }
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
        if persistRecentlyHeard {
            saveHeardHistory()
        }
    }

    // MARK: - ReflectorObserver (callback from tokio background task)

    public nonisolated func onEvent(event: ReflectorEvent) {
        // Yield into the serial mailbox; the single main-actor pump
        // task (started in `init`) applies events strictly in arrival
        // order so `@Observable` change tracking fires on the same
        // actor SwiftUI is rendering on and voiceFrame can never
        // overtake its voiceStart.
        eventContinuation.yield(event)
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
            audioPlayer.beginKeepAlive()

        case .disconnected(let reason):
            state = .disconnected
            let wasUserInitiated = userInitiatedDisconnect
            userInitiatedDisconnect = false
            let reasonText = reason.displayText
            lastError = "reflector disconnected: \(reasonText)"
            // No endKeepAlive here for unexpected drops: the audio
            // pipeline must keep the process alive through the
            // reconnect backoff so a backgrounded app can relink.
            // The backoff task releases it if all attempts fail;
            // user-initiated disconnects released it already.
            // Flush decoder state; the tail PCM is discarded because the
            // link is gone and there's nothing left to play through.
            _ = rxPipeline.endStream()
            finalizeCurrentStream(reason: reasonText)
            if !wasUserInitiated {
                NotificationManager.shared.reflectorDisconnected(reason: reasonText)
                scheduleUnexpectedReconnect()
            }

        case .pollEcho:
            break

        case .voiceStart(let streamId, let mycall, let suffix, let urcall, let rpt1, let rpt2, _):
            // `_` = headerBytes. ReflectorCoordinator shows metadata
            // only; `RelayCoordinator` consumes the raw header to
            // forward to the radio as an MMDVM DStarHeader frame.
            rxPipeline.startStream()
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

        case .voiceFrame(let streamId, let seq, let voiceBytes):
            if var s = currentStream, s.id == streamId {
                s.framesReceived &+= 1
                currentStream = s
            }
            let pcm = rxPipeline.pushVoice(seq: seq, voiceBytes: voiceBytes)
            audioPlayer.enqueue(pcm)

        case .slowDataUpdate(let streamId, let text, let position):
            if var s = currentStream, s.id == streamId {
                s.latestText = text
                s.latestPosition = position
                currentStream = s
            }

        case .voiceEnd(let streamId, let reason, let text, let position):
            let streamEnd = rxPipeline.endStream()
            audioPlayer.enqueue(streamEnd.tailPcm)
            lastStreamStats = streamEnd.stats
            if let s = currentStream, s.id == streamId {
                // Prefer the authoritative values carried on VoiceEnd;
                // fall back to whatever the snapshot last saw.
                var endReason = reason.displayText
                if streamEnd.stats.lost > 0 {
                    endReason += " · \(streamEnd.stats.lost) lost"
                }
                appendHeard(
                    from: s,
                    endReason: endReason,
                    text: text ?? s.latestText,
                    position: position ?? s.latestPosition
                )
            }
            currentStream = nil

        case .unknown(let detail):
            log.warning("Unrecognised reflector event: \(detail)")

        case .ended:
            state = .disconnected
            session = nil
            connectedReflector = nil
            audioPlayer.endKeepAlive()
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
        if persistRecentlyHeard {
            saveHeardHistory()
        }
    }

    // MARK: - Heard-history persistence

    /// Codable projection of `HeardEntry` for UserDefaults archival.
    /// We can't directly encode `HeardEntry` because it embeds
    /// `GpsPosition` (a UniFFI-generated type without Codable).
    /// `id` is optional so archives written before it existed still
    /// decode (they get fresh UUIDs on load, once).
    private struct PersistedHeard: Codable {
        let id: UUID?
        let mycall: String
        let suffix: String
        let urcall: String
        let endedAt: Date
        let duration: TimeInterval
        let frames: UInt32
        let endReason: String
        let text: String?
        let lat: Double?
        let lon: Double?
        let callsign: String?
        let symbol: String?
        let comment: String?

        init(from entry: HeardEntry) {
            self.id = entry.id
            self.mycall = entry.mycall
            self.suffix = entry.suffix
            self.urcall = entry.urcall
            self.endedAt = entry.endedAt
            self.duration = entry.duration
            self.frames = entry.frames
            self.endReason = entry.endReason
            self.text = entry.text
            self.lat = entry.position?.latitude
            self.lon = entry.position?.longitude
            self.callsign = entry.position?.callsign
            self.symbol = entry.position?.symbol
            self.comment = entry.position?.comment
        }

        func toEntry() -> HeardEntry {
            let position: GpsPosition?
            if let lat, let lon {
                position = GpsPosition(
                    callsign: callsign ?? "",
                    latitude: lat,
                    longitude: lon,
                    symbol: symbol ?? "",
                    comment: comment
                )
            } else {
                position = nil
            }
            return HeardEntry(
                id: id ?? UUID(),
                mycall: mycall,
                suffix: suffix,
                urcall: urcall,
                endedAt: endedAt,
                duration: duration,
                frames: frames,
                endReason: endReason,
                text: text,
                position: position
            )
        }
    }

    private func saveHeardHistory() {
        let archive = recentlyHeard.map(PersistedHeard.init(from:))
        guard let data = try? JSONEncoder().encode(archive) else { return }
        UserDefaults.standard.set(data, forKey: Self.heardHistoryKey)
    }

    private static func loadHeardHistory() -> [HeardEntry] {
        guard let data = UserDefaults.standard.data(forKey: Self.heardHistoryKey),
              let archive = try? JSONDecoder().decode([PersistedHeard].self, from: data)
        else {
            return []
        }
        return archive.map { $0.toEntry() }
    }

    /// Schedule a best-effort reconnect after the reflector drops us
    /// unexpectedly (network blip, reflector restart, keepalive
    /// timeout). Backoff: 5s / 15s / 45s — mirrors the pattern
    /// `tryAutoConnect` uses for launch-time stale-session recovery.
    ///
    /// Only fires when the previous link was a user-picked reflector
    /// (so we know where to reconnect to). Cancels any prior pending
    /// reconnect task before scheduling — prevents stacked retries
    /// during a rapid flap.
    private func scheduleUnexpectedReconnect() {
        guard let reflector = connectedReflector else { return }
        reconnectTask?.cancel()
        reconnectTask = Task { @MainActor [weak self] in
            let delays: [UInt64] = [5_000_000_000, 15_000_000_000, 45_000_000_000]
            for delay in delays {
                try? await Task.sleep(nanoseconds: delay)
                guard let self else { return }
                guard Task.isCancelled == false else { return }
                // User may have picked a different reflector, or
                // toggled off auto-reconnect preconditions. Re-check.
                guard self.session == nil else { return }
                guard case .disconnected = self.state else { return }
                await self.connect(to: reflector)
                if case .connected = self.state {
                    NotificationManager.shared.reflectorReconnected(name: reflector.name)
                    return
                }
            }
            // Backoff exhausted — release the audio keep-alive so the
            // app can suspend instead of rendering silence forever for
            // a link that isn't coming back.
            guard let self, !Task.isCancelled else { return }
            self.audioPlayer.endKeepAlive()
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
        public let id: UUID
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

        /// `id` defaults to a fresh UUID for newly-heard entries; the
        /// persistence layer passes the stored one back in so SwiftUI
        /// list identity survives quits and relaunches.
        public init(
            id: UUID = UUID(),
            mycall: String,
            suffix: String,
            urcall: String,
            endedAt: Date,
            duration: TimeInterval,
            frames: UInt32,
            endReason: String,
            text: String?,
            position: GpsPosition?
        ) {
            self.id = id
            self.mycall = mycall
            self.suffix = suffix
            self.urcall = urcall
            self.endedAt = endedAt
            self.duration = duration
            self.frames = frames
            self.endReason = endReason
            self.text = text
            self.position = position
        }
    }
}
