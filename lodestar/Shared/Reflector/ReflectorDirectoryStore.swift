// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "directory")

/// Owns the merged reflector directory: bundled hosts files plus the
/// cached results of on-demand DPlus auth-server refreshes. Merging and
/// provenance precedence live in Rust (`mergeDirectories`); this store
/// handles the platform pieces: cache file and observability.
@Observable
@MainActor
public final class ReflectorDirectoryStore {
    public private(set) var entries: [DirectoryEntry]
    public private(set) var statusLine: String
    public private(set) var isRefreshing = false

    /// Flat reflector list for pickers.
    public var reflectors: [Reflector] { entries.map(\.reflector) }

    private let cacheUrl: URL?
    private let bundledEntries: [DirectoryEntry]

    /// Cached row: mirrors `DirectoryEntry` with Codable types.
    private struct CachedEntry: Codable {
        let name: String
        let host: String
        let port: UInt16
        let protocolName: String
        let sourceName: String
    }

    private struct CacheFile: Codable {
        let fetchedAt: Date
        let entries: [CachedEntry]
    }

    /// `cacheUrl: nil` disables persistence (tests, previews).
    /// Default production location:
    /// `<Application Support>/Lodestar/reflectors.json`.
    public init(cacheUrl: URL? = ReflectorDirectoryStore.defaultCacheUrl()) {
        self.cacheUrl = cacheUrl
        let bundled = defaultReflectors().map {
            DirectoryEntry(reflector: $0, source: .bundled)
        }
        self.bundledEntries = bundled
        var merged = bundled
        var status = "\(bundled.count) reflectors · bundled list"
        if let cacheUrl,
           let data = try? Data(contentsOf: cacheUrl),
           let cache = try? JSONDecoder().decode(CacheFile.self, from: data) {
            // Only DPlus-auth rows are accepted. Older releases wrote XLX
            // registry rows to this same cache; ignoring every other source
            // prevents those plaintext-derived addresses from influencing a
            // connection after upgrade.
            let cached = cache.entries.compactMap(Self.dPlusEntry(from:))
            if !cached.isEmpty {
                merged = mergeDirectories(entries: bundled + cached)
                status = "\(merged.count) reflectors · last DPlus update \(cache.fetchedAt.formatted(date: .abbreviated, time: .shortened))"
            }
        }
        self.entries = merged
        self.statusLine = status
    }

    /// `<Application Support>/Lodestar/reflectors.json`, creating the
    /// directory on first use. `nil` when Application Support is
    /// unavailable (sandbox misconfiguration).
    public static func defaultCacheUrl() -> URL? {
        guard let base = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first else { return nil }
        let dir = base.appendingPathComponent("Lodestar", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("reflectors.json")
    }

    /// Fetch the DPlus auth list, then re-merge and cache its REF rows.
    public func refreshDPlusDirectory(callsign: String?) async {
        guard !isRefreshing else { return }
        guard let callsign, !callsign.isEmpty else {
            statusLine = "\(entries.count) reflectors · enter a callsign to refresh DPlus"
            return
        }
        isRefreshing = true
        defer { isRefreshing = false }
        do {
            let refs = try await fetchDplusDirectory(callsign: callsign)
            integrateDPlus(fetched: refs)
            statusLine = "\(entries.count) reflectors · DPlus updated \(Date.now.formatted(date: .abbreviated, time: .shortened))"
        } catch {
            statusLine = "\(entries.count) reflectors · DPlus auth list failed"
            log.error("DPlus directory fetch failed: \(error)")
        }
    }

    /// Replace the prior DPlus-auth snapshot with freshly fetched rows and
    /// persist. Rows absent from a later response must not linger.
    /// Internal so tests can drive it without network.
    func integrateDPlus(fetched: [Reflector]) {
        let tagged = fetched
            .filter { $0.protocol == .dPlus }
            .map { DirectoryEntry(reflector: $0, source: .dPlusAuth) }
        // Always merge against the complete bundled snapshot. A previous
        // auth row may have shadowed its bundled counterpart in `entries`;
        // filtering the merged view would lose that fallback when a later
        // auth response omits the row.
        entries = mergeDirectories(entries: bundledEntries + tagged)
        saveCache()
    }

    private func saveCache() {
        guard let cacheUrl else { return }
        // Persist only DPlus-auth rows. Bundled entries reload from the
        // binary, and legacy XLX-derived rows must never be written again.
        let rows = entries
            .filter { $0.source == .dPlusAuth }
            .map(Self.cached(from:))
        let file = CacheFile(fetchedAt: .now, entries: rows)
        guard let data = try? JSONEncoder().encode(file) else { return }
        try? FileManager.default.createDirectory(
            at: cacheUrl.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? data.write(to: cacheUrl, options: .atomic)
    }

    private static func cached(from entry: DirectoryEntry) -> CachedEntry {
        CachedEntry(
            name: entry.reflector.name,
            host: entry.reflector.host,
            port: entry.reflector.port,
            protocolName: protocolName(entry.reflector.protocol),
            sourceName: "auth"
        )
    }

    private static func dPlusEntry(from cached: CachedEntry) -> DirectoryEntry? {
        guard cached.sourceName == "auth",
              cached.protocolName == "dplus" else { return nil }
        return DirectoryEntry(
            reflector: Reflector(
                name: cached.name, host: cached.host, port: cached.port,
                protocol: .dPlus, description: ""
            ),
            source: .dPlusAuth
        )
    }

    private static func protocolName(_ p: ReflectorProtocol) -> String {
        switch p {
        case .dPlus: return "dplus"
        case .dExtra: return "dextra"
        case .dcs: return "dcs"
        }
    }
}
