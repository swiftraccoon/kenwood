//! D-STAR reflector host file parser.
//!
//! Host files map reflector names (e.g. "REF030", "XRF012", "DCS003")
//! to their network addresses and ports. The format is one entry per
//! line: `name address port` or `name address` (Pi-Star format).
//!
//! Lines starting with `#` are comments. Empty lines are skipped.
//!
//! Host lists come from the XLX self-registration registry
//! (`xlxapi.rlx.lu`) and the `DPlus` auth server; see
//! [`parse_xlx_directory`] for the registry format.

use std::collections::HashMap;

use crate::types::ProtocolKind;

/// A resolved reflector host entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// Reflector name (e.g. "REF030").
    pub name: String,
    /// Hostname or IP address.
    pub address: String,
    /// UDP port number.
    pub port: u16,
}

/// Collection of host file entries keyed by reflector name.
///
/// Lookups are case-insensitive — the query is upper-cased before
/// the `HashMap` lookup. Parsed insertion likewise upper-cases the
/// name. Duplicate names use last-wins semantics.
#[derive(Debug, Clone, Default)]
pub struct HostFile {
    entries: HashMap<String, HostEntry>,
}

impl HostFile {
    /// Create an empty host file.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse host entries from text content.
    ///
    /// Supports two formats:
    /// - 3 columns: `name address port`
    /// - 2 columns: `name address` (port from `default_port`)
    ///
    /// Lines starting with `#` are comments. Empty lines skipped.
    /// Unparseable ports fall back to `default_port` rather than
    /// dropping the entry.
    pub fn parse(&mut self, content: &str, default_port: u16) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(name_raw) = parts.next() else {
                continue;
            };
            let Some(address) = parts.next() else {
                tracing::debug!(line, "host file: line has fewer than 2 fields, skipped");
                continue;
            };
            let port = parts
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(default_port);
            let name = name_raw.to_ascii_uppercase();
            drop(self.entries.insert(
                name.clone(),
                HostEntry {
                    name,
                    address: address.to_owned(),
                    port,
                },
            ));
        }
    }

    /// Look up an entry by name (case-insensitive).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&HostEntry> {
        self.entries.get(&name.to_ascii_uppercase())
    }

    /// Insert a host entry directly. Uses `entry.name` verbatim as
    /// the key, so callers should pass an upper-case name to remain
    /// reachable via [`Self::lookup`].
    pub fn insert(&mut self, entry: HostEntry) {
        drop(self.entries.insert(entry.name.clone(), entry));
    }

    /// Number of entries in the host file.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the host file has zero entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate all entries.
    pub fn iter(&self) -> impl Iterator<Item = &HostEntry> + '_ {
        self.entries.values()
    }
}

/// Default UDP port for `DPlus` (`REF`) reflectors.
const XLX_DPLUS_PORT: u16 = 20001;
/// Default UDP port for `DExtra` (`XRF`) reflectors.
const XLX_DEXTRA_PORT: u16 = 30001;
/// Default UDP port for `DCS` reflectors.
const XLX_DCS_PORT: u16 = 30051;

/// Parse the XLX reflector directory into protocol-tagged host entries.
///
/// This is the `xlxapi.rlx.lu/api.php?do=GetReflectorHostname` format:
/// one `name<whitespace>address` entry per line, grouped under `#`
/// comment headers. The same reflector appears once per protocol
/// prefix (`REF`, `XRF`, `DCS`), all pointing at one address — those
/// prefixes are protocols, not separate networks.
///
/// Protocol family and UDP port are derived from the name prefix.
/// Lines with an unrecognised prefix, comment lines (`#`), blank
/// lines, and lines with fewer than two fields are skipped.
#[must_use]
pub fn parse_xlx_directory(content: &str) -> Vec<(ProtocolKind, HostEntry)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name_raw), Some(address)) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name_raw.to_ascii_uppercase();
        let (protocol, port) = if name.starts_with("REF") {
            (ProtocolKind::DPlus, XLX_DPLUS_PORT)
        } else if name.starts_with("XRF") {
            (ProtocolKind::DExtra, XLX_DEXTRA_PORT)
        } else if name.starts_with("DCS") {
            (ProtocolKind::Dcs, XLX_DCS_PORT)
        } else {
            continue;
        };
        out.push((
            protocol,
            HostEntry {
                name,
                address: address.to_owned(),
                port,
            },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_three_column_format() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 ref001.dstargateway.org 20001\n", 0);
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(entry.name, "REF001");
        assert_eq!(entry.address, "ref001.dstargateway.org");
        assert_eq!(entry.port, 20001);
        Ok(())
    }

    #[test]
    fn parse_two_column_format_uses_default_port() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("XRF012 xrf012.dstar.su\n", 30001);
        let entry = hf.lookup("XRF012").ok_or("XRF012 present")?;
        assert_eq!(entry.port, 30001);
        Ok(())
    }

    #[test]
    fn parse_skips_comments() {
        let mut hf = HostFile::new();
        hf.parse("# this is a comment\nREF001 a 20001\n", 0);
        assert!(hf.lookup("REF001").is_some());
        assert_eq!(hf.len(), 1);
    }

    #[test]
    fn parse_skips_empty_lines() {
        let mut hf = HostFile::new();
        hf.parse("\n\nREF001 a 20001\n\n", 0);
        assert_eq!(hf.len(), 1);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut hf = HostFile::new();
        hf.parse("REF001 a 20001\n", 0);
        assert!(hf.lookup("ref001").is_some());
        assert!(hf.lookup("Ref001").is_some());
    }

    #[test]
    fn parse_duplicates_last_wins() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 first 20001\nREF001 second 20002\n", 0);
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(entry.address, "second");
        Ok(())
    }

    #[test]
    fn parse_unparseable_port_falls_back_to_default() -> TestResult {
        let mut hf = HostFile::new();
        hf.parse("REF001 a notaport\n", 12345);
        let entry = hf.lookup("REF001").ok_or("REF001 present")?;
        assert_eq!(entry.port, 12345);
        Ok(())
    }

    #[test]
    fn parse_short_line_skipped() {
        let mut hf = HostFile::new();
        hf.parse("REF001\n", 0);
        assert_eq!(hf.len(), 0);
    }

    #[test]
    fn xlx_directory_routes_prefix_to_protocol_and_port() {
        let sample = "\
# XLX live Reflector host file.
#
# XLX-DCS Reflectors
#
DCS030\t185.230.132.103

# XLX-XRF Reflectors
XRF030\t185.230.132.103

# XLX-REF Reflectors
REF030\t185.230.132.103
";
        let parsed = parse_xlx_directory(sample);
        assert_eq!(parsed.len(), 3, "three reflectors parsed, got {parsed:?}");
        assert!(parsed.contains(&(
            ProtocolKind::Dcs,
            HostEntry {
                name: "DCS030".to_owned(),
                address: "185.230.132.103".to_owned(),
                port: 30051,
            },
        )));
        assert!(parsed.contains(&(
            ProtocolKind::DExtra,
            HostEntry {
                name: "XRF030".to_owned(),
                address: "185.230.132.103".to_owned(),
                port: 30001,
            },
        )));
        assert!(parsed.contains(&(
            ProtocolKind::DPlus,
            HostEntry {
                name: "REF030".to_owned(),
                address: "185.230.132.103".to_owned(),
                port: 20001,
            },
        )));
    }

    #[test]
    fn xlx_directory_skips_unknown_prefix_and_short_lines() -> TestResult {
        // `XLX307` (the registry emits no bare XLX prefix), a
        // one-field line, and a comment must all be skipped.
        let sample = "XLX307\t1.2.3.4\nREF001\nDCS001\t9.9.9.9\n# trailer\n";
        let parsed = parse_xlx_directory(sample);
        assert_eq!(
            parsed.len(),
            1,
            "only DCS001 is a valid 2-field known-prefix line"
        );
        let entry = parsed.first().ok_or("one entry parsed")?;
        assert_eq!(entry.1.name, "DCS001");
        Ok(())
    }
}
