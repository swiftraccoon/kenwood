// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import Observation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "directory")

/// Owns the merged reflector directory: bundled hosts files, the
/// cached results of previous fetches, and on-demand refreshes from
/// the XLX registry + DPlus auth server. Merging and provenance
/// precedence live in Rust (`mergeDirectories`); this store handles
/// the platform pieces: HTTP, cache file, observability.
@Observable
@MainActor
public final class ReflectorDirectoryStore {
    public private(set) var entries: [DirectoryEntry]
    public private(set) var statusLine: String
    public private(set) var isRefreshing = false

    /// Flat reflector list for pickers.
    public var reflectors: [Reflector] { entries.map(\.reflector) }

    private let cacheUrl: URL?

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
        var merged = bundled
        var status = "\(bundled.count) reflectors · bundled list"
        if let cacheUrl,
           let data = try? Data(contentsOf: cacheUrl),
           let cache = try? JSONDecoder().decode(CacheFile.self, from: data) {
            let cached = cache.entries.compactMap(Self.entry(from:))
            merged = mergeDirectories(entries: bundled + cached)
            status = "\(merged.count) reflectors · last fetched \(cache.fetchedAt.formatted(date: .abbreviated, time: .shortened))"
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

    /// Fetch the XLX registry (always) and the DPlus auth list (when a
    /// callsign is available), then re-merge and cache.
    public func refresh(callsign: String?) async {
        guard !isRefreshing else { return }
        isRefreshing = true
        defer { isRefreshing = false }
        var problems: [String] = []

        if let url = URL(string: xlxDirectoryUrl()) {
            do {
                let (data, _) = try await URLSession.shared.data(from: url)
                let text = String(decoding: data, as: UTF8.self)
                integrate(fetched: parseXlxText(text: text), source: .xlxRegistry)
            } catch {
                problems.append("XLX fetch failed")
                log.error("XLX directory fetch failed: \(error)")
            }
        }

        if let callsign, !callsign.isEmpty {
            do {
                let refs = try await fetchDplusDirectory(callsign: callsign)
                integrate(fetched: refs, source: .dPlusAuth)
            } catch {
                problems.append("DPlus auth list failed")
                log.error("DPlus directory fetch failed: \(error)")
            }
        }

        var status = "\(entries.count) reflectors · updated \(Date.now.formatted(date: .abbreviated, time: .shortened))"
        if !problems.isEmpty {
            status += " · \(problems.joined(separator: ", "))"
        }
        statusLine = status
    }

    /// Merge freshly fetched rows into the directory and persist.
    /// Internal so tests can drive it without network.
    func integrate(fetched: [Reflector], source: DirectorySource) {
        guard !fetched.isEmpty else { return }
        let tagged = fetched.map { DirectoryEntry(reflector: $0, source: source) }
        entries = mergeDirectories(entries: entries + tagged)
        saveCache()
    }

    private func saveCache() {
        guard let cacheUrl else { return }
        // Persist only non-bundled rows; bundled entries reload from
        // the binary and must not shadow a future bundled-list update.
        let rows = entries
            .filter { $0.source != .bundled }
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
            sourceName: sourceName(entry.source)
        )
    }

    private static func entry(from cached: CachedEntry) -> DirectoryEntry? {
        guard let proto = protocolValue(cached.protocolName),
              let source = sourceValue(cached.sourceName) else { return nil }
        return DirectoryEntry(
            reflector: Reflector(
                name: cached.name, host: cached.host, port: cached.port,
                protocol: proto, description: ""
            ),
            source: source
        )
    }

    private static func protocolName(_ p: ReflectorProtocol) -> String {
        switch p {
        case .dPlus: return "dplus"
        case .dExtra: return "dextra"
        case .dcs: return "dcs"
        }
    }

    private static func protocolValue(_ s: String) -> ReflectorProtocol? {
        switch s {
        case "dplus": return .dPlus
        case "dextra": return .dExtra
        case "dcs": return .dcs
        default: return nil
        }
    }

    private static func sourceName(_ s: DirectorySource) -> String {
        switch s {
        case .bundled: return "bundled"
        case .dPlusAuth: return "auth"
        case .xlxRegistry: return "xlx"
        }
    }

    private static func sourceValue(_ s: String) -> DirectorySource? {
        switch s {
        case "bundled": return .bundled
        case "auth": return .dPlusAuth
        case "xlx": return .xlxRegistry
        default: return nil
        }
    }
}
