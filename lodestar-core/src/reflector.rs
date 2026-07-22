// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Reflector types exposed across the Rust-to-Swift FFI boundary.
//!
//! The default reflector list is parsed at startup from the
//! ircDDBGateway Pi-Star hosts files bundled as `data/DPlus_Hosts.txt`,
//! `data/DExtra_Hosts.txt`, and `data/DCS_Hosts.txt`. These are the
//! same files `thd75-repl` consumes and match the community-maintained
//! hosts list on <https://hosts.pistar.uk/hosts/>.

use std::sync::OnceLock;

use dstar_gateway_core::codec::{dcs, dextra, dplus};
use dstar_gateway_core::hosts::{HostFile, parse_xlx_directory};
use dstar_gateway_core::types::ProtocolKind as CoreProtocolKind;

/// Which D-STAR reflector protocol a given host speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectorProtocol {
    /// `DPlus`: REF-style reflectors (originated by Icom, widely deployed).
    DPlus,
    /// `DExtra`: XRF-style reflectors.
    DExtra,
    /// DCS-style reflectors.
    Dcs,
}

impl From<CoreProtocolKind> for ReflectorProtocol {
    fn from(value: CoreProtocolKind) -> Self {
        match value {
            CoreProtocolKind::DExtra => Self::DExtra,
            CoreProtocolKind::Dcs => Self::Dcs,
            // DPlus, plus any future core protocol kinds, map to DPlus.
            _ => Self::DPlus,
        }
    }
}

impl From<ReflectorProtocol> for CoreProtocolKind {
    fn from(value: ReflectorProtocol) -> Self {
        match value {
            ReflectorProtocol::DPlus => Self::DPlus,
            ReflectorProtocol::DExtra => Self::DExtra,
            ReflectorProtocol::Dcs => Self::Dcs,
        }
    }
}

/// A single reflector the user can link to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reflector {
    /// Uppercase reflector callsign prefix, e.g. `REF030`, `XRF030`, `DCS001`.
    pub name: String,
    /// DNS hostname or IPv4 literal for the reflector.
    pub host: String,
    /// UDP port the reflector listens on.
    pub port: u16,
    /// Which D-STAR protocol this reflector speaks.
    pub protocol: ReflectorProtocol,
    /// Optional human-readable notes (location, sponsor, etc.).
    pub description: String,
}

/// Bundled hosts files from `ircDDBGateway/Data/`. Small enough
/// (1309 lines total) to embed in the binary, which saves a network fetch
/// on first launch and means the list always works offline.
const DPLUS_HOSTS: &str = include_str!("../data/DPlus_Hosts.txt");
const DEXTRA_HOSTS: &str = include_str!("../data/DExtra_Hosts.txt");
const DCS_HOSTS: &str = include_str!("../data/DCS_Hosts.txt");

/// Reorder hosts-file lines so that, for any reflector listed both by
/// IP literal and by hostname, the hostname row comes LAST. `HostFile`
/// collapses duplicate names with last-row-wins semantics during
/// parse, so this ordering is what makes the hostname survive: a
/// hostname outlives the reflector changing address, unlike a pinned
/// IP. (The bundled files really do mix orders: `XRF000` lists its
/// hostname last, `DCS006` lists its IP last.)
fn prefer_hostname_ordering(text: &str) -> String {
    let (ip_rows, host_rows): (Vec<&str>, Vec<&str>) = text
        .lines()
        .partition(|line| line.split_whitespace().nth(1).is_some_and(is_ip_literal));
    let mut out = String::with_capacity(text.len() + 1);
    for line in ip_rows.into_iter().chain(host_rows) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Parse one bundled hosts file into tagged reflector rows. Duplicate
/// names are collapsed by `HostFile` (last row wins); the text is
/// pre-ordered so hostname rows win over IP literals.
fn hostfile_reflectors(
    text: &str,
    default_port: u16,
    protocol: ReflectorProtocol,
) -> Vec<Reflector> {
    let mut file = HostFile::new();
    file.parse(&prefer_hostname_ordering(text), default_port);
    file.iter()
        .map(|e| Reflector {
            name: e.name.clone(),
            host: e.address.clone(),
            port: e.port,
            protocol,
            description: String::new(),
        })
        .collect()
}

/// Parse the bundled hosts files once and cache the result for all
/// subsequent `default_reflectors()` calls.
fn parsed_hosts() -> &'static Vec<Reflector> {
    static CACHE: OnceLock<Vec<Reflector>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out: Vec<Reflector> = Vec::new();
        out.extend(hostfile_reflectors(
            DPLUS_HOSTS,
            dplus::consts::DEFAULT_PORT,
            ReflectorProtocol::DPlus,
        ));
        out.extend(hostfile_reflectors(
            DEXTRA_HOSTS,
            dextra::consts::DEFAULT_PORT,
            ReflectorProtocol::DExtra,
        ));
        out.extend(hostfile_reflectors(
            DCS_HOSTS,
            dcs::consts::DEFAULT_PORT,
            ReflectorProtocol::Dcs,
        ));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    })
}

/// Cheap heuristic: if every character is a digit or dot, treat as IP.
fn is_ip_literal(host: &str) -> bool {
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ':')
}

/// Returns the full curated list of well-known public reflectors (REF,
/// XRF, DCS), merged from the three Pi-Star hosts files.
#[must_use]
pub fn default_reflectors() -> Vec<Reflector> {
    parsed_hosts().clone()
}

/// Where a directory entry came from.
///
/// Merge precedence: `DPlusAuth` beats `Bundled` beats `XlxRegistry`.
/// The auth server is the authoritative REF directory, and XLX's
/// `REFnnn` names are aliases of XLX reflectors rather than the
/// dstargateway.org REFs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DirectorySource {
    /// Compiled-in Pi-Star hosts files.
    Bundled,
    /// Live `DPlus` auth-server host list.
    DPlusAuth,
    /// XLX self-registration registry.
    XlxRegistry,
}

/// A reflector tagged with the source it came from.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DirectoryEntry {
    /// The reflector row.
    pub reflector: Reflector,
    /// Which source produced it.
    pub source: DirectorySource,
}

/// Merge priority for a source; lower wins. `DPlusAuth` (0) beats
/// `Bundled` (1) beats `XlxRegistry` (2).
const fn source_priority(source: DirectorySource) -> u8 {
    match source {
        DirectorySource::DPlusAuth => 0,
        DirectorySource::Bundled => 1,
        DirectorySource::XlxRegistry => 2,
    }
}

/// Parse a Pi-Star-format hosts file for one protocol. Unknown lines
/// are skipped (same lenient parser as the bundled lists).
#[uniffi::export]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary requires owned String"
)]
#[must_use]
pub fn parse_hosts_text(protocol: ReflectorProtocol, text: String) -> Vec<Reflector> {
    let default_port = match protocol {
        ReflectorProtocol::DPlus => dplus::consts::DEFAULT_PORT,
        ReflectorProtocol::DExtra => dextra::consts::DEFAULT_PORT,
        ReflectorProtocol::Dcs => dcs::consts::DEFAULT_PORT,
    };
    let mut file = HostFile::new();
    file.parse(&text, default_port);
    file.iter()
        .map(|e| Reflector {
            name: e.name.clone(),
            host: e.address.clone(),
            port: e.port,
            protocol,
            description: String::new(),
        })
        .collect()
}

/// Parse the XLX registry response (`xlx_directory_url()` body) into
/// tagged reflector rows.
#[uniffi::export]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary requires owned String"
)]
#[must_use]
pub fn parse_xlx_text(text: String) -> Vec<Reflector> {
    parse_xlx_directory(&text)
        .into_iter()
        .map(|(kind, e)| Reflector {
            name: e.name,
            host: e.address,
            port: e.port,
            protocol: kind.into(),
            description: String::new(),
        })
        .collect()
}

/// URL of the XLX self-registration registry (fetched by the app
/// layer with the platform HTTP stack, then parsed via
/// [`parse_xlx_text`]).
#[uniffi::export]
#[must_use]
pub fn xlx_directory_url() -> String {
    "http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname".to_owned()
}

/// Stable ordering for same-name entries across protocols, so the
/// merged list renders identically run to run.
const fn protocol_rank(protocol: ReflectorProtocol) -> u8 {
    match protocol {
        ReflectorProtocol::DPlus => 0,
        ReflectorProtocol::DExtra => 1,
        ReflectorProtocol::Dcs => 2,
    }
}

/// Merge tagged entries: one row per `(name, protocol)`, source
/// priority `DPlusAuth` > `Bundled` > `XlxRegistry`, sorted by name
/// then protocol.
#[uniffi::export]
#[must_use]
pub fn merge_directories(entries: Vec<DirectoryEntry>) -> Vec<DirectoryEntry> {
    let mut best: std::collections::HashMap<(String, ReflectorProtocol), DirectoryEntry> =
        std::collections::HashMap::new();
    for e in entries {
        let key = (e.reflector.name.clone(), e.reflector.protocol);
        let winner = best
            .get(&key)
            .is_none_or(|held| source_priority(e.source) < source_priority(held.source));
        if winner {
            drop(best.insert(key, e));
        }
    }
    let mut out: Vec<DirectoryEntry> = best.into_values().collect();
    out.sort_by(|a, b| {
        a.reflector.name.cmp(&b.reflector.name).then_with(|| {
            protocol_rank(a.reflector.protocol).cmp(&protocol_rank(b.reflector.protocol))
        })
    });
    out
}

#[cfg(test)]
mod tests {
    use super::{
        DirectoryEntry, DirectorySource, Reflector, ReflectorProtocol, default_reflectors,
        is_ip_literal, merge_directories, parse_hosts_text,
    };
    use dstar_gateway_core::types::ProtocolKind as CoreProtocolKind;

    fn entry(
        name: &str,
        host: &str,
        proto: ReflectorProtocol,
        source: DirectorySource,
    ) -> DirectoryEntry {
        DirectoryEntry {
            reflector: Reflector {
                name: name.to_owned(),
                host: host.to_owned(),
                port: 20001,
                protocol: proto,
                description: String::new(),
            },
            source,
        }
    }

    #[test]
    fn merge_prefers_auth_over_bundled_over_xlx() -> Result<(), Box<dyn std::error::Error>> {
        let merged = merge_directories(vec![
            entry(
                "REF030",
                "bundled.example",
                ReflectorProtocol::DPlus,
                DirectorySource::Bundled,
            ),
            entry(
                "REF030",
                "auth.example",
                ReflectorProtocol::DPlus,
                DirectorySource::DPlusAuth,
            ),
            entry(
                "REF030",
                "xlx.example",
                ReflectorProtocol::DPlus,
                DirectorySource::XlxRegistry,
            ),
        ]);
        assert_eq!(merged.len(), 1);
        let only = merged.first().ok_or("empty merge")?;
        assert_eq!(only.reflector.host, "auth.example");
        assert_eq!(only.source, DirectorySource::DPlusAuth);
        Ok(())
    }

    #[test]
    fn bundled_duplicates_prefer_hostname_over_ip_literal() {
        // The bundled files list some reflectors twice (IP + hostname)
        // in BOTH row orders: XRF000 has its hostname last, DCS006 has
        // its IP last. Both must resolve to the hostname.
        let list = default_reflectors();
        let dcs006 = list
            .iter()
            .find(|r| r.name == "DCS006" && r.protocol == ReflectorProtocol::Dcs);
        assert!(
            matches!(dcs006, Some(r) if r.host == "dcs006.xreflector.net"),
            "DCS006 must keep its hostname row, got {dcs006:?}"
        );
        let xrf000 = list
            .iter()
            .find(|r| r.name == "XRF000" && r.protocol == ReflectorProtocol::DExtra);
        assert!(
            matches!(xrf000, Some(r) if r.host == "000.xreflector.org"),
            "XRF000 must keep its hostname row, got {xrf000:?}"
        );
    }

    #[test]
    fn merge_precedence_holds_regardless_of_input_order() -> Result<(), Box<dyn std::error::Error>>
    {
        // The XLX row arrives FIRST here; auth must still win.
        let merged = merge_directories(vec![
            entry(
                "REF030",
                "xlx.example",
                ReflectorProtocol::DPlus,
                DirectorySource::XlxRegistry,
            ),
            entry(
                "REF030",
                "auth.example",
                ReflectorProtocol::DPlus,
                DirectorySource::DPlusAuth,
            ),
        ]);
        assert_eq!(merged.len(), 1);
        let only = merged.first().ok_or("empty merge")?;
        assert_eq!(
            only.reflector.host, "auth.example",
            "auth must win even when it arrives after the XLX row"
        );
        Ok(())
    }

    #[test]
    fn merge_keeps_distinct_protocols_apart() {
        let merged = merge_directories(vec![
            entry(
                "XRF030",
                "a.example",
                ReflectorProtocol::DExtra,
                DirectorySource::Bundled,
            ),
            entry(
                "REF030",
                "b.example",
                ReflectorProtocol::DPlus,
                DirectorySource::Bundled,
            ),
        ]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn parse_hosts_text_round_trips_a_line() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_hosts_text(
            ReflectorProtocol::DExtra,
            "XRF999    xrf999.example.org\n".to_owned(),
        );
        assert_eq!(parsed.len(), 1);
        let first = parsed.first().ok_or("no entry parsed")?;
        assert_eq!(first.name, "XRF999");
        assert_eq!(first.port, 30001);
        Ok(())
    }

    #[test]
    fn bundled_dplus_list_is_deduped_by_name() {
        let dplus: Vec<_> = default_reflectors()
            .into_iter()
            .filter(|r| r.protocol == ReflectorProtocol::DPlus)
            .collect();
        let names: std::collections::HashSet<&str> =
            dplus.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names.len(),
            dplus.len(),
            "duplicate DPlus names in bundled list"
        );
    }

    #[test]
    fn default_reflectors_are_nonempty() {
        let list = default_reflectors();
        assert!(!list.is_empty(), "default reflector list must not be empty");
    }

    #[test]
    fn default_reflectors_contain_known_reflectors() {
        let list = default_reflectors();
        let names: Vec<&str> = list.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"REF030"), "missing REF030");
        assert!(names.contains(&"XRF030"), "missing XRF030");
        assert!(names.contains(&"DCS001"), "missing DCS001");
    }

    #[test]
    fn default_reflectors_cover_all_three_protocols() {
        let protocols: std::collections::HashSet<ReflectorProtocol> = default_reflectors()
            .into_iter()
            .map(|r| r.protocol)
            .collect();
        assert!(protocols.contains(&ReflectorProtocol::DPlus));
        assert!(protocols.contains(&ReflectorProtocol::DExtra));
        assert!(protocols.contains(&ReflectorProtocol::Dcs));
    }

    #[test]
    fn default_reflectors_have_sane_ports() {
        for r in default_reflectors() {
            assert_ne!(r.port, 0, "{} has zero port", r.name);
            match r.protocol {
                ReflectorProtocol::DPlus => {
                    assert_eq!(r.port, 20001, "{} wrong DPlus port", r.name);
                }
                ReflectorProtocol::DExtra => {
                    assert_eq!(r.port, 30001, "{} wrong DExtra port", r.name);
                }
                ReflectorProtocol::Dcs => assert_eq!(r.port, 30051, "{} wrong DCS port", r.name),
            }
        }
    }

    #[test]
    fn protocol_kind_round_trips() {
        for p in [
            ReflectorProtocol::DPlus,
            ReflectorProtocol::DExtra,
            ReflectorProtocol::Dcs,
        ] {
            let core: CoreProtocolKind = p.into();
            let back: ReflectorProtocol = core.into();
            assert_eq!(p, back, "round-trip lost info for {p:?}");
        }
    }

    #[test]
    fn ip_literal_detection() {
        assert!(is_ip_literal("192.168.1.1"));
        assert!(is_ip_literal("44.103.34.3"));
        assert!(!is_ip_literal("ref030.dstargateway.org"));
        assert!(!is_ip_literal("xrf030.oe3xht.at"));
        assert!(!is_ip_literal(""));
    }
}
