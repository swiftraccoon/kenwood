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
use dstar_gateway_core::hosts::HostFile;
use dstar_gateway_core::types::ProtocolKind as CoreProtocolKind;

/// Which D-STAR reflector protocol a given host speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectorProtocol {
    /// `DPlus` — REF-style reflectors (originated by Icom, widely deployed).
    DPlus,
    /// `DExtra` — XRF-style reflectors.
    DExtra,
    /// DCS-style reflectors.
    Dcs,
}

impl From<CoreProtocolKind> for ReflectorProtocol {
    fn from(value: CoreProtocolKind) -> Self {
        #[expect(
            clippy::match_same_arms,
            reason = "explicit DPlus arm + fallback wildcard is intentional for future-proofing"
        )]
        match value {
            CoreProtocolKind::DPlus => Self::DPlus,
            CoreProtocolKind::DExtra => Self::DExtra,
            CoreProtocolKind::Dcs => Self::Dcs,
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
    /// Uppercase reflector callsign prefix — e.g. `REF030`, `XRF030`, `DCS001`.
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

/// Bundled hosts files from `ref/ircDDBGateway/Data/`. Small enough
/// (1309 lines total) to embed in the binary — saves a network fetch
/// on first launch and means the list always works offline.
const DPLUS_HOSTS: &str = include_str!("../data/DPlus_Hosts.txt");
const DEXTRA_HOSTS: &str = include_str!("../data/DExtra_Hosts.txt");
const DCS_HOSTS: &str = include_str!("../data/DCS_Hosts.txt");

/// Parse the bundled hosts files once and cache the result for all
/// subsequent `default_reflectors()` calls.
fn parsed_hosts() -> &'static Vec<Reflector> {
    static CACHE: OnceLock<Vec<Reflector>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out: Vec<Reflector> = Vec::new();

        let mut dplus = HostFile::new();
        dplus.parse(DPLUS_HOSTS, dplus::consts::DEFAULT_PORT);
        for e in dplus.iter() {
            out.push(Reflector {
                name: e.name.clone(),
                host: e.address.clone(),
                port: e.port,
                protocol: ReflectorProtocol::DPlus,
                description: String::new(),
            });
        }

        let mut dextra = HostFile::new();
        dextra.parse(DEXTRA_HOSTS, dextra::consts::DEFAULT_PORT);
        // Hosts files can list the same reflector twice (IP + hostname).
        // Dedupe by name, prefer hostname entries (non-numeric first char).
        let mut dextra_by_name: std::collections::HashMap<String, Reflector> =
            std::collections::HashMap::new();
        for e in dextra.iter() {
            let r = Reflector {
                name: e.name.clone(),
                host: e.address.clone(),
                port: e.port,
                protocol: ReflectorProtocol::DExtra,
                description: String::new(),
            };
            // Prefer entries whose first host-char is a letter (hostname
            // rather than raw IP). Replace only if the existing entry is
            // an IP and the new one is a name.
            let keep_new = dextra_by_name
                .get(&e.name)
                .is_none_or(|existing| is_ip_literal(&existing.host) && !is_ip_literal(&r.host));
            if keep_new {
                drop(dextra_by_name.insert(e.name.clone(), r));
            }
        }
        out.extend(dextra_by_name.into_values());

        let mut dcs = HostFile::new();
        dcs.parse(DCS_HOSTS, dcs::consts::DEFAULT_PORT);
        let mut dcs_by_name: std::collections::HashMap<String, Reflector> =
            std::collections::HashMap::new();
        for e in dcs.iter() {
            let r = Reflector {
                name: e.name.clone(),
                host: e.address.clone(),
                port: e.port,
                protocol: ReflectorProtocol::Dcs,
                description: String::new(),
            };
            let keep_new = dcs_by_name
                .get(&e.name)
                .is_none_or(|existing| is_ip_literal(&existing.host) && !is_ip_literal(&r.host));
            if keep_new {
                drop(dcs_by_name.insert(e.name.clone(), r));
            }
        }
        out.extend(dcs_by_name.into_values());

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

#[cfg(test)]
mod tests {
    use super::{ReflectorProtocol, default_reflectors, is_ip_literal};
    use dstar_gateway_core::types::ProtocolKind as CoreProtocolKind;

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
