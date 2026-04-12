//! D-STAR reflector host file parser.
//!
//! Host files map reflector names (e.g. "REF030", "XRF012", "DCS003")
//! to their network addresses and ports. The format is one entry per
//! line: `name address port` or `name address` (Pi-Star format).
//!
//! Lines starting with `#` are comments. Empty lines are skipped.
//!
//! Sources for host files:
//! - <https://hosts.pistar.uk/hosts/>
//! - Local reflector operators

use std::collections::HashMap;

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
}
