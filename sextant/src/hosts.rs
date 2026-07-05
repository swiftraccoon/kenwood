// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Reflector directory — the searchable list of reflectors offered in
//! the connection panel.
//!
//! Sources, in merge-precedence order: the bundled POLARIS test
//! reflector, the XLX self-registration registry (fetched and cached),
//! and the `DPlus` auth `HostList` (merged opportunistically after a
//! `DPlus` connect). Entries are deduplicated by `(callsign, protocol)`;
//! the first source to provide a pair wins.

use std::fmt::Write as _;

use dstar_gateway_core::types::ProtocolKind;

/// Where a directory entry came from. Decides dedup precedence and
/// is shown in the picker so a surprising address is explainable.
///
/// The `REFnnn` namespace is ambiguous: the dstargateway auth server
/// lists the legacy US-network REF reflectors, while the XLX
/// self-registration registry lists `REFnnn` as the DPlus-protocol
/// alias of XLX reflector `nnn` — a different server entirely. The
/// auth server is authoritative for `REF` names, so its entries must
/// win the dedup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostSource {
    /// Compiled-in (the POLARIS test reflector).
    Bundled,
    /// The `DPlus` auth server's host list — authoritative for `REF`.
    DPlusAuth,
    /// XLX self-registration registry. A `REFnnn` from here is an
    /// XLX in `DPlus`-compat mode, not a dstargateway REF.
    XlxRegistry,
}

impl HostSource {
    /// Short provenance tag for the directory picker row.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::DPlusAuth => "dstargateway",
            Self::XlxRegistry => "xlx registry",
        }
    }
}

/// One reflector the connection panel can pre-fill.
#[derive(Debug, Clone)]
pub(crate) struct ReflectorHost {
    /// Reflector callsign.
    pub(crate) callsign: String,
    /// Host name or IP.
    pub(crate) host: String,
    /// UDP port.
    pub(crate) port: u16,
    /// Protocol family.
    pub(crate) protocol: ProtocolKind,
    /// Which directory source provided this entry.
    pub(crate) source: HostSource,
}

/// The bundled host list — just the local POLARIS test reflector,
/// which is in no public registry and so must ship with the app.
pub(crate) fn bundled() -> Vec<ReflectorHost> {
    vec![ReflectorHost {
        callsign: "POLARIS".into(),
        host: "127.0.0.1".into(),
        port: 30001,
        protocol: ProtocolKind::DExtra,
        source: HostSource::Bundled,
    }]
}

/// Result of a background directory fetch, delivered to the GUI over
/// the directory channel.
#[derive(Debug)]
pub(crate) enum DirectoryUpdate {
    /// An XLX-registry fetch succeeded — the parsed hosts and a
    /// display timestamp.
    Loaded {
        /// Reflectors parsed from the registry.
        hosts: Vec<ReflectorHost>,
        /// ISO-date display string for the fetch time.
        when: String,
    },
    /// A `DPlus` auth-server fetch succeeded — the authoritative REF
    /// host list, merged at higher precedence than the XLX registry.
    AuthLoaded {
        /// REF reflectors from the auth server.
        hosts: Vec<ReflectorHost>,
    },
    /// A fetch failed — the error text for the status line.
    Failed(String),
}

/// UTC date (`YYYY-MM-DD`) for stamping a fresh fetch.
fn today_utc() -> String {
    time::OffsetDateTime::now_utc()
        .format(time::macros::format_description!("[year]-[month]-[day]"))
        .unwrap_or_else(|_| "unknown".into())
}

/// Fetch the XLX reflector directory. Runs on the tokio runtime; the
/// caller forwards the returned [`DirectoryUpdate`] to the GUI.
pub(crate) async fn fetch_directory() -> DirectoryUpdate {
    let fetcher = dstar_gateway::hosts_fetcher::HostsFetcher::new();
    match fetcher.fetch_xlx_directory().await {
        Ok(entries) => {
            let hosts = entries
                .into_iter()
                .map(|(protocol, entry)| ReflectorHost {
                    callsign: entry.name,
                    host: entry.address,
                    port: entry.port,
                    protocol,
                    source: HostSource::XlxRegistry,
                })
                .collect();
            DirectoryUpdate::Loaded {
                hosts,
                when: today_utc(),
            }
        }
        Err(e) => DirectoryUpdate::Failed(e.to_string()),
    }
}

/// Fetch the authoritative REF host list from the `DPlus` auth
/// server (the same TCP exchange every `DPlus` dongle performs at
/// startup). Runs on the tokio runtime; the caller forwards the
/// returned [`DirectoryUpdate`] to the GUI. Failures are reported
/// but harmless — the directory keeps its other sources.
pub(crate) async fn fetch_auth_directory(callsign: String) -> DirectoryUpdate {
    let callsign = match dstar_gateway_core::types::Callsign::try_from_str(callsign.trim()) {
        Ok(c) => c,
        Err(e) => return DirectoryUpdate::Failed(format!("auth callsign {callsign:?}: {e}")),
    };
    match dstar_gateway::auth::AuthClient::new()
        .authenticate(callsign)
        .await
    {
        Ok(host_list) => {
            let hosts = host_list
                .hosts()
                .iter()
                .map(|h| ReflectorHost {
                    callsign: h.callsign.clone(),
                    host: h.address.to_string(),
                    port: 20001,
                    protocol: ProtocolKind::DPlus,
                    source: HostSource::DPlusAuth,
                })
                .collect();
            DirectoryUpdate::AuthLoaded { hosts }
        }
        Err(e) => DirectoryUpdate::Failed(format!("dstargateway auth list: {e}")),
    }
}

/// The merged, deduplicated reflector directory.
pub(crate) struct ReflectorDirectory {
    /// Reflectors from the XLX registry fetch (or its cached copy).
    fetched: Vec<ReflectorHost>,
    /// Reflectors merged from the `DPlus` auth host list.
    merged: Vec<ReflectorHost>,
    /// Combined, deduplicated view — rebuilt whenever `fetched` or
    /// `merged` changes, so a per-frame `search` is a cheap filter.
    hosts: Vec<ReflectorHost>,
    /// Human-readable provenance line for the connection panel.
    status: String,
}

impl ReflectorDirectory {
    /// A directory with no fetched/merged entries — just the bundled
    /// POLARIS reflector.
    pub(crate) fn bundled_only() -> Self {
        Self {
            fetched: Vec::new(),
            merged: Vec::new(),
            hosts: bundled(),
            status: "reflector list: not fetched yet".into(),
        }
    }

    /// Replace the fetched (XLX) entries; merged `DPlus`-auth entries
    /// are preserved. `when` is a display string for the fetch time.
    pub(crate) fn replace_fetched(&mut self, fetched: Vec<ReflectorHost>, when: &str) {
        self.status = format!(
            "reflector list: {} reflectors · fetched {when}",
            fetched.len()
        );
        self.fetched = fetched;
        self.rebuild();
    }

    /// Merge additional entries (the `DPlus` auth `HostList`) in.
    /// Merged entries outrank the XLX-fetched ones on rebuild.
    pub(crate) fn merge_hosts(&mut self, extra: Vec<ReflectorHost>) {
        self.merged.extend(extra);
        self.rebuild();
    }

    /// Rebuild the combined view: bundled, then auth-merged, then
    /// XLX-fetched, deduplicated by `(callsign, protocol)` — the
    /// earliest wins. Auth entries outrank the XLX registry because
    /// XLX self-registrations reuse `REFnnn` names that collide with
    /// the genuine dstargateway reflectors of the same name.
    fn rebuild(&mut self) {
        let mut all = bundled();
        all.extend(self.merged.iter().cloned());
        all.extend(self.fetched.iter().cloned());
        dedup_hosts(&mut all);
        self.hosts = all;
    }

    /// Reflectors whose callsign contains `query` (case-insensitive).
    /// An empty query returns the whole directory.
    pub(crate) fn search(&self, query: &str) -> Vec<&ReflectorHost> {
        let q = query.trim().to_ascii_uppercase();
        self.hosts
            .iter()
            .filter(|h| q.is_empty() || h.callsign.to_ascii_uppercase().contains(&q))
            .collect()
    }

    /// The provenance line for display.
    pub(crate) fn status(&self) -> &str {
        &self.status
    }

    /// Overwrite the provenance line (transient states: "fetching…",
    /// fetch error).
    pub(crate) fn set_status(&mut self, status: String) {
        self.status = status;
    }

    /// Load the directory from the on-disk cache, falling back to
    /// [`Self::bundled_only`] when the cache is missing or unreadable.
    pub(crate) fn load_cached() -> Self {
        let Some(path) = cache_path() else {
            return Self::bundled_only();
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::bundled_only();
        };
        let (fetched, when) = from_tsv(&content);
        if fetched.is_empty() {
            return Self::bundled_only();
        }
        let mut dir = Self::bundled_only();
        dir.replace_fetched(fetched, when.as_deref().unwrap_or("unknown"));
        dir.status = format!("{} (cached)", dir.status);
        dir
    }

    /// Write the fetched entries to the on-disk cache. Errors are
    /// logged at `warn` and swallowed — caching must never block the UI.
    pub(crate) fn save_cache(&self, when: &str) {
        let Some(path) = cache_path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(path = %parent.display(), error = %e, "reflector cache: mkdir failed");
            return;
        }
        if let Err(e) = std::fs::write(&path, to_tsv(&self.fetched, when)) {
            tracing::warn!(path = %path.display(), error = %e, "reflector cache: write failed");
        }
    }
}

/// Remove later duplicates sharing an `(uppercased callsign, protocol)`
/// key — the first occurrence wins.
fn dedup_hosts(hosts: &mut Vec<ReflectorHost>) {
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert((h.callsign.to_ascii_uppercase(), format!("{:?}", h.protocol))));
}

/// Resolve the reflector-directory cache file path. `None` when no
/// config directory is available (e.g. headless CI).
fn cache_path() -> Option<std::path::PathBuf> {
    let mut dir = dirs_next::config_dir()?;
    dir.push("sextant");
    Some(dir.join("reflectors.tsv"))
}

/// Serialize hosts to the cache TSV: a `# fetched <when>` header line
/// followed by one `callsign<TAB>host<TAB>port<TAB>protocol` line each.
fn to_tsv(hosts: &[ReflectorHost], when: &str) -> String {
    let mut out = format!("# fetched {when}\n");
    for h in hosts {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{:?}",
            h.callsign, h.host, h.port, h.protocol
        );
    }
    out
}

/// Parse the cache TSV. Returns the hosts and the `when` string from
/// the header (if present). Malformed lines are skipped.
fn from_tsv(content: &str) -> (Vec<ReflectorHost>, Option<String>) {
    let mut hosts = Vec::new();
    let mut when = None;
    for line in content.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("# fetched ") {
            when = Some(rest.trim().to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(callsign), Some(host), Some(port_s), Some(proto_s)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(port) = port_s.parse::<u16>() else {
            continue;
        };
        let protocol = match proto_s {
            "DPlus" => ProtocolKind::DPlus,
            "Dcs" | "DCS" => ProtocolKind::Dcs,
            "DExtra" => ProtocolKind::DExtra,
            _ => continue,
        };
        hosts.push(ReflectorHost {
            callsign: callsign.to_owned(),
            host: host.to_owned(),
            port,
            protocol,
            // The cache persists only XLX-registry fetches.
            source: HostSource::XlxRegistry,
        });
    }
    (hosts, when)
}

#[cfg(test)]
mod tests {
    use super::{HostSource, ReflectorDirectory, ReflectorHost, bundled};
    use dstar_gateway_core::types::ProtocolKind;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn host(callsign: &str, protocol: ProtocolKind, port: u16, addr: &str) -> ReflectorHost {
        ReflectorHost {
            callsign: callsign.to_owned(),
            host: addr.to_owned(),
            port,
            protocol,
            source: HostSource::XlxRegistry,
        }
    }

    #[test]
    fn bundled_list_is_non_empty_and_polaris_first() {
        let hosts = bundled();
        assert!(!hosts.is_empty());
        let Some(first) = hosts.first() else {
            return;
        };
        assert_eq!(first.callsign, "POLARIS");
    }

    #[test]
    fn replace_fetched_keeps_bundled_and_adds_fetched() {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_fetched(
            vec![host("REF030", ProtocolKind::DPlus, 20001, "1.2.3.4")],
            "2026-05-16",
        );
        assert!(
            dir.search("POLARIS")
                .iter()
                .any(|h| h.callsign == "POLARIS")
        );
        assert!(dir.search("REF030").iter().any(|h| h.callsign == "REF030"));
    }

    #[test]
    fn merge_survives_a_later_replace_fetched() {
        // A DPlus-auth merge followed by a fresh XLX fetch: the refetch
        // must not discard the merged entries.
        let mut dir = ReflectorDirectory::bundled_only();
        dir.merge_hosts(vec![host("REF999", ProtocolKind::DPlus, 20001, "9.9.9.9")]);
        dir.replace_fetched(
            vec![host("REF030", ProtocolKind::DPlus, 20001, "1.2.3.4")],
            "2026-05-16",
        );
        assert_eq!(
            dir.search("REF999").len(),
            1,
            "merged entry survived the refetch"
        );
        assert_eq!(dir.search("REF030").len(), 1, "fetched entry present");
    }

    /// The `REFnnn` namespace collides: XLX self-registrations reuse
    /// names of genuine dstargateway reflectors. The auth-merged
    /// entry (authoritative for REF) must win the dedup, or "REF030"
    /// routes to an unrelated XLX in DPlus-compat mode.
    #[test]
    fn dedup_prefers_auth_merged_over_fetched() -> TestResult {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_fetched(
            vec![host("REF030", ProtocolKind::DPlus, 20001, "xlx-imposter")],
            "2026-05-16",
        );
        let mut authoritative = host("REF030", ProtocolKind::DPlus, 20001, "dstargateway");
        authoritative.source = HostSource::DPlusAuth;
        dir.merge_hosts(vec![authoritative]);
        let hits = dir.search("REF030");
        assert_eq!(hits.len(), 1, "deduped to one entry, got {hits:?}");
        let first = hits.first().ok_or("one REF030 hit")?;
        assert_eq!(
            first.host, "dstargateway",
            "the auth-merged entry wins the tie"
        );
        Ok(())
    }

    #[test]
    fn search_matches_callsign_substring_case_insensitively() {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_fetched(
            vec![
                host("REF030", ProtocolKind::DPlus, 20001, "1.1.1.1"),
                host("XRF030", ProtocolKind::DExtra, 30001, "1.1.1.1"),
                host("DCS001", ProtocolKind::Dcs, 30051, "2.2.2.2"),
            ],
            "2026-05-16",
        );
        assert_eq!(dir.search("030").len(), 2, "two reflectors numbered 030");
        assert_eq!(dir.search("ref030").len(), 1, "case-insensitive match");
    }

    #[test]
    fn tsv_roundtrip_preserves_hosts() -> TestResult {
        let hosts = vec![
            host("REF030", ProtocolKind::DPlus, 20001, "1.2.3.4"),
            host("XRF012", ProtocolKind::DExtra, 30001, "5.6.7.8"),
            host("DCS001", ProtocolKind::Dcs, 30051, "9.9.9.9"),
        ];
        let tsv = super::to_tsv(&hosts, "2026-05-16");
        let (parsed, when) = super::from_tsv(&tsv);
        assert_eq!(when.as_deref(), Some("2026-05-16"));
        assert_eq!(parsed.len(), 3);
        let ref030 = parsed
            .iter()
            .find(|h| h.callsign == "REF030")
            .ok_or("REF030 round-tripped")?;
        assert_eq!(ref030.protocol, ProtocolKind::DPlus);
        assert_eq!(ref030.port, 20001);
        Ok(())
    }

    #[test]
    fn from_tsv_skips_malformed_lines() {
        let tsv = "# fetched 2026-05-16\nREF001\t1.2.3.4\t20001\tDPlus\nbad line\n";
        let (parsed, _) = super::from_tsv(tsv);
        assert_eq!(parsed.len(), 1);
    }
}
