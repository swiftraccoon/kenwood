//! D-STAR reflector host file parser.
//!
//! Host files map reflector names (e.g. "REF030", "XRF012", "DCS003")
//! to their network addresses and ports. The format is one entry per
//! line: `name hostname port`.
//!
//! Host files can be obtained from:
//! - <http://xlx.n5amd.com/index.php?show=hosts>
//! - <https://www.pistar.uk/downloads/>
//! - Local reflector operators
//!
//! # File format
//!
//! ```text
//! REF001  ref001.dstargateway.org  20001
//! XRF012  xrf012.dstar.su          30001
//! DCS003  dcs003.xreflector.net    30051
//! ```
//!
//! Lines starting with `#` are comments. Empty lines are skipped.

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
    /// - 2 columns: `name address` (Pi-Star format, port from `default_port`)
    ///
    /// Lines starting with `#` are comments. Empty lines are skipped.
    pub fn parse(&mut self, content: &str, default_port: u16) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }

            let name = parts[0].to_uppercase();
            let address = parts[1].to_string();
            let port = parts
                .get(2)
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(default_port);

            let _ = self.entries.insert(
                name.clone(),
                HostEntry {
                    name,
                    address,
                    port,
                },
            );
        }
    }

    /// Load and parse a host file from disk.
    ///
    /// The `default_port` is used when entries don't specify a port
    /// (Pi-Star 2-column format).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be read.
    pub fn load(path: &std::path::Path, default_port: u16) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let mut hf = Self::new();
        hf.parse(&content, default_port);
        Ok(hf)
    }

    /// Look up a reflector by name (case-insensitive).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&HostEntry> {
        self.entries.get(&name.to_uppercase())
    }

    /// Number of entries in the host file.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the host file is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = &HostEntry> {
        self.entries.values()
    }

    /// Insert a host entry manually (e.g. from `DPlus` auth response).
    pub fn insert(&mut self, entry: HostEntry) {
        let _ = self.entries.insert(entry.name.clone(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let mut hf = HostFile::new();
        hf.parse("REF030 ref030.dstargateway.org 20001\n", 0);
        assert_eq!(hf.len(), 1);
        let entry = hf.lookup("REF030").unwrap();
        assert_eq!(entry.address, "ref030.dstargateway.org");
        assert_eq!(entry.port, 20001);
    }

    #[test]
    fn parse_case_insensitive() {
        let mut hf = HostFile::new();
        hf.parse("ref030 ref030.example.com 20001\n", 0);
        assert!(hf.lookup("REF030").is_some());
        assert!(hf.lookup("ref030").is_some());
    }

    #[test]
    fn parse_comments_and_blanks() {
        let mut hf = HostFile::new();
        hf.parse(
            "# comment\n\nREF001 host1 20001\n# another\nXRF002 host2 30001\n",
            0,
        );
        assert_eq!(hf.len(), 2);
    }

    #[test]
    fn parse_two_column_with_default_port() {
        let mut hf = HostFile::new();
        hf.parse("XRF030\t47.206.136.225\n", 30001);
        assert_eq!(hf.len(), 1);
        let entry = hf.lookup("XRF030").unwrap();
        assert_eq!(entry.address, "47.206.136.225");
        assert_eq!(entry.port, 30001);
    }

    #[test]
    fn parse_single_field_skipped() {
        let mut hf = HostFile::new();
        hf.parse("REF001\n", 20001);
        assert_eq!(hf.len(), 0);
    }

    #[test]
    fn parse_invalid_port_uses_default() {
        let mut hf = HostFile::new();
        hf.parse("REF001 host1 notaport\n", 20001);
        assert_eq!(hf.len(), 1);
        assert_eq!(hf.lookup("REF001").unwrap().port, 20001);
    }

    #[test]
    fn empty_host_file() {
        let hf = HostFile::new();
        assert!(hf.is_empty());
        assert_eq!(hf.len(), 0);
        assert!(hf.lookup("REF030").is_none());
    }
}
