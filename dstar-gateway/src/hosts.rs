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
///
/// # Semantics
///
/// - Lookups via [`HostFile::lookup`] are case-insensitive: the query
///   is upper-cased before the `HashMap` lookup, so `lookup("ref030")`
///   and `lookup("REF030")` find the same entry.
/// - Parsed insertion via [`HostFile::parse`] upper-cases the name
///   before inserting, so both `REF030 ...` and `ref030 ...` map to
///   the same key `"REF030"`.
/// - Manual insertion via [`HostFile::insert`] uses
///   `HostEntry::name` verbatim as the key. Inserting an entry whose
///   `name` is mixed-case produces a key that is only reachable
///   through `lookup` if its upper-cased form matches, so callers
///   should pass upper-case callsigns when inserting manually.
/// - Duplicate callsigns use last-wins semantics: the most recently
///   parsed (or inserted) entry replaces any earlier entry with the
///   same key.
/// - Malformed lines in [`HostFile::parse`] (fewer than 2
///   whitespace-separated fields) are logged at
///   `tracing::debug!` and skipped. Unparseable port values fall
///   back to the supplied `default_port` rather than skipping the
///   entry.
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
                tracing::debug!(
                    line = %line,
                    reason = "fewer than 2 whitespace-separated fields",
                    "host file parse error, skipping line"
                );
                continue;
            }

            let name = parts[0].to_uppercase();
            let address = parts[1].to_string();
            // The third column is historically the port number, but
            // Pi-Star-flavoured host files use single-letter status
            // flags in that column instead:
            //   `L` — locked / link-only (no user dial-in)
            //   `S` — selfcare / status page
            //   `U` — unused
            // None of these are port numbers. When we see a known
            // non-port flag, silently fall back to default_port. Only
            // genuinely unexpected content produces a debug log.
            let port = parts.get(2).map_or(default_port, |raw| {
                raw.parse::<u16>().unwrap_or_else(|_| {
                    if !matches!(*raw, "L" | "S" | "U") {
                        // Unknown non-numeric value — log for visibility.
                        // Known flags fall through silently.
                        tracing::debug!(
                            line = %line,
                            raw_port = %raw,
                            "host file port not a number, using default_port"
                        );
                    }
                    default_port
                })
            });

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
    /// Returns [`crate::Error::Io`] if the file cannot be read.
    pub fn load(path: &std::path::Path, default_port: u16) -> Result<Self, crate::Error> {
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
    fn parse_pistar_lock_flag_uses_default_port() {
        // Pi-Star host files put "L" (locked / link-only) in the port
        // column for reflectors that don't accept user dial-in. We
        // should treat this as "no port specified" and silently fall
        // back to default_port, without a warning log.
        let mut hf = HostFile::new();
        hf.parse("W4LCO\t208.89.76.92\tL\nDCS005\t44.31.166.9\tL\n", 20001);
        assert_eq!(hf.len(), 2);
        assert_eq!(hf.lookup("W4LCO").unwrap().port, 20001);
        assert_eq!(hf.lookup("W4LCO").unwrap().address, "208.89.76.92");
        assert_eq!(hf.lookup("DCS005").unwrap().port, 20001);
    }

    #[test]
    fn parse_pistar_status_flags_also_accepted() {
        // `S` (selfcare) and `U` (unused) are other Pi-Star flag values
        // that appear in the port column. Treat them the same as `L`.
        let mut hf = HostFile::new();
        hf.parse("REFX1 host1.example S\nREFX2 host2.example U\n", 20001);
        assert_eq!(hf.len(), 2);
        assert_eq!(hf.lookup("REFX1").unwrap().port, 20001);
        assert_eq!(hf.lookup("REFX2").unwrap().port, 20001);
    }

    #[test]
    fn empty_host_file() {
        let hf = HostFile::new();
        assert!(hf.is_empty());
        assert_eq!(hf.len(), 0);
        assert!(hf.lookup("REF030").is_none());
    }
}
