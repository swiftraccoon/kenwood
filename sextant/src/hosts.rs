// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Reflector directory: the searchable list of reflectors offered in
//! the connection panel.
//!
//! Sources are the bundled POLARIS test reflector and the `DPlus` auth
//! `HostList`, fetched at startup or merged opportunistically after a
//! `DPlus` connect. Entries are deduplicated by `(callsign, protocol)`;
//! the authoritative auth entry wins a collision.

use dstar_gateway_core::types::ProtocolKind;

/// Where a directory entry came from. Decides dedup precedence and
/// is shown in the picker so a surprising address is explainable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostSource {
    /// Compiled-in (the POLARIS test reflector).
    Bundled,
    /// The `DPlus` auth server's host list, authoritative for `REF`.
    DPlusAuth,
}

impl HostSource {
    /// Short provenance tag for the directory picker row.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::DPlusAuth => "dstargateway",
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

/// The bundled host list: just the local POLARIS test reflector,
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

/// Result of a background `DPlus` directory fetch, delivered to the GUI over
/// the directory channel.
#[derive(Debug)]
pub(crate) enum DirectoryUpdate {
    /// A `DPlus` auth-server fetch succeeded, carrying the authoritative REF
    /// host list.
    AuthLoaded {
        /// REF reflectors from the auth server.
        hosts: Vec<ReflectorHost>,
    },
    /// The auth fetch failed; carries the error text for the status line.
    AuthFailed(String),
}

/// Fetch the authoritative REF host list from the `DPlus` auth
/// server (the same TCP exchange every `DPlus` dongle performs at
/// startup). Runs on the tokio runtime; the caller forwards the
/// returned [`DirectoryUpdate`] to the GUI. Failures are reported
/// but harmless: the bundled directory remains available.
pub(crate) async fn fetch_auth_directory(callsign: String) -> DirectoryUpdate {
    let callsign = match dstar_gateway_core::types::Callsign::try_from_str(callsign.trim()) {
        Ok(c) => c,
        Err(e) => {
            return DirectoryUpdate::AuthFailed(format!("auth callsign {callsign:?}: {e}"));
        }
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
        Err(e) => DirectoryUpdate::AuthFailed(format!("dstargateway auth list: {e}")),
    }
}

/// The merged, deduplicated reflector directory.
pub(crate) struct ReflectorDirectory {
    /// Reflectors merged from the `DPlus` auth host list.
    merged: Vec<ReflectorHost>,
    /// Combined, deduplicated view, rebuilt whenever `merged` changes,
    /// so a per-frame `search` is a cheap filter.
    hosts: Vec<ReflectorHost>,
    /// Human-readable provenance line for the connection panel.
    status: String,
}

impl ReflectorDirectory {
    /// A directory with no fetched entries: just the bundled POLARIS
    /// reflector. Legacy generated directory caches are deliberately not read.
    pub(crate) fn bundled_only() -> Self {
        let hosts = bundled();
        Self {
            merged: Vec::new(),
            status: format!("reflector list: {} bundled", hosts.len()),
            hosts,
        }
    }

    /// Replace the entries from the `DPlus` auth `HostList` with a fresh
    /// snapshot. Entries absent from a later response must not linger.
    pub(crate) fn replace_auth_hosts(&mut self, hosts: Vec<ReflectorHost>) {
        self.merged = hosts;
        self.rebuild();
        self.status = format!(
            "reflector list: {} reflectors · bundled + dstargateway",
            self.hosts.len()
        );
    }

    /// Rebuild the combined view: auth-merged, then bundled, deduplicated by
    /// `(callsign, protocol)`, where the authoritative auth entry wins.
    fn rebuild(&mut self) {
        let mut all = self.merged.clone();
        all.extend(bundled());
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

    /// Overwrite the provenance line while an auth fetch is active or after it
    /// fails.
    pub(crate) fn set_status(&mut self, status: String) {
        self.status = status;
    }
}

/// Remove later duplicates sharing an `(uppercased callsign, protocol)`
/// key; the first occurrence wins.
fn dedup_hosts(hosts: &mut Vec<ReflectorHost>) {
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert((h.callsign.to_ascii_uppercase(), format!("{:?}", h.protocol))));
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
            source: HostSource::DPlusAuth,
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
    fn replace_auth_keeps_bundled_and_adds_fetched() {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_auth_hosts(vec![host("REF030", ProtocolKind::DPlus, 20001, "1.2.3.4")]);
        assert!(
            dir.search("POLARIS")
                .iter()
                .any(|h| h.callsign == "POLARIS")
        );
        assert!(dir.search("REF030").iter().any(|h| h.callsign == "REF030"));
    }

    #[test]
    fn auth_refresh_replaces_changed_and_removed_entries() -> TestResult {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_auth_hosts(vec![
            host("REF030", ProtocolKind::DPlus, 20001, "old.example"),
            host("REF888", ProtocolKind::DPlus, 20001, "removed.example"),
        ]);
        dir.replace_auth_hosts(vec![host(
            "REF030",
            ProtocolKind::DPlus,
            20001,
            "fresh.example",
        )]);

        assert!(dir.search("REF888").is_empty(), "removed auth row lingered");
        let refreshed = dir.search("REF030");
        let first = refreshed.first().ok_or("refreshed auth row missing")?;
        assert_eq!(first.host, "fresh.example");
        Ok(())
    }

    #[test]
    fn dedup_prefers_auth_over_bundled() -> TestResult {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_auth_hosts(vec![host(
            "POLARIS",
            ProtocolKind::DExtra,
            30001,
            "dstargateway",
        )]);
        let hits = dir.search("POLARIS");
        assert_eq!(hits.len(), 1, "deduped to one entry, got {hits:?}");
        let first = hits.first().ok_or("one POLARIS hit")?;
        assert_eq!(first.host, "dstargateway", "the auth entry wins the tie");
        Ok(())
    }

    #[test]
    fn search_matches_callsign_substring_case_insensitively() {
        let mut dir = ReflectorDirectory::bundled_only();
        dir.replace_auth_hosts(vec![
            host("REF030", ProtocolKind::DPlus, 20001, "1.1.1.1"),
            host("XRF030", ProtocolKind::DExtra, 30001, "1.1.1.1"),
            host("DCS001", ProtocolKind::Dcs, 30051, "2.2.2.2"),
        ]);
        assert_eq!(dir.search("030").len(), 2, "two reflectors numbered 030");
        assert_eq!(dir.search("ref030").len(), 1, "case-insensitive match");
    }
}
