//! D-STAR reflector host file parser.
//!
//! Parses `DExtra`, `DCS`, and `DPlus` host files used by MMDVM and related
//! D-STAR infrastructure. These are simple text files that map reflector
//! names to IP addresses and optional port numbers.
//!
//! # File format
//!
//! Each line contains: `NAME<whitespace>ADDRESS [PORT]`
//!
//! - Lines starting with `#` are comments.
//! - Empty lines are ignored.
//! - If no port is specified, a protocol-specific default is used.
//!
//! # Example
//!
//! ```text
//! # DExtra reflectors
//! REF001    1.2.3.4
//! REF002    5.6.7.8  30001
//! ```

/// Well-known default port for `DExtra` reflector connections.
pub const DEXTRA_PORT: u16 = 30001;

/// Well-known default port for `DCS` reflector connections.
pub const DCS_PORT: u16 = 30051;

/// Well-known default port for `DPlus` reflector connections.
pub const DPLUS_PORT: u16 = 20001;

/// A parsed reflector host entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// Reflector name (e.g., `"REF001"`).
    pub name: String,
    /// IP address or hostname of the reflector.
    pub address: String,
    /// Connection port.
    pub port: u16,
}

/// Parse a host file (`DExtra`, `DCS`, or `DPlus` format).
///
/// Each non-empty, non-comment line is parsed as:
/// `NAME<whitespace>ADDRESS [PORT]`
///
/// If a line does not specify a port, `default_port` is used.
/// Malformed lines (fewer than 2 fields) are silently skipped.
///
/// # Parameters
///
/// - `content`: The full text content of the host file.
/// - `default_port`: Port to use when the line omits the port field.
///   Use [`DEXTRA_PORT`], [`DCS_PORT`], or [`DPLUS_PORT`] as appropriate.
#[must_use]
pub fn parse_host_file(content: &str, default_port: u16) -> Vec<HostEntry> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();

            // Skip empty lines and comments.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }

            let mut fields = trimmed.split_whitespace();
            let name = fields.next()?;
            let address = fields.next()?;
            let port = fields
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(default_port);

            Some(HostEntry {
                name: name.to_owned(),
                address: address.to_owned(),
                port,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_entries() {
        let content = "REF001    1.2.3.4\nREF002    5.6.7.8\n";
        let entries = parse_host_file(content, DEXTRA_PORT);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "REF001");
        assert_eq!(entries[0].address, "1.2.3.4");
        assert_eq!(entries[0].port, DEXTRA_PORT);
        assert_eq!(entries[1].name, "REF002");
        assert_eq!(entries[1].address, "5.6.7.8");
        assert_eq!(entries[1].port, DEXTRA_PORT);
    }

    #[test]
    fn parse_with_explicit_port() {
        let content = "REF001    1.2.3.4  30099\n";
        let entries = parse_host_file(content, DEXTRA_PORT);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].port, 30099);
    }

    #[test]
    fn parse_skips_comments_and_empty_lines() {
        let content = "# This is a comment\n\n  # indented comment\nREF001  1.2.3.4\n\n";
        let entries = parse_host_file(content, DCS_PORT);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "REF001");
        assert_eq!(entries[0].port, DCS_PORT);
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let content = "REF001\nREF002  5.6.7.8\nJUSTONEFIELD\n";
        let entries = parse_host_file(content, DPLUS_PORT);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "REF002");
        assert_eq!(entries[0].port, DPLUS_PORT);
    }

    #[test]
    fn parse_empty_content() {
        let entries = parse_host_file("", DEXTRA_PORT);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_only_comments() {
        let entries = parse_host_file("# comment 1\n# comment 2\n", DEXTRA_PORT);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_dcs_default_port() {
        let content = "DCS001  10.0.0.1\n";
        let entries = parse_host_file(content, DCS_PORT);

        assert_eq!(entries[0].port, DCS_PORT);
    }

    #[test]
    fn parse_dplus_default_port() {
        let content = "REF001  10.0.0.1\n";
        let entries = parse_host_file(content, DPLUS_PORT);

        assert_eq!(entries[0].port, DPLUS_PORT);
    }

    #[test]
    fn parse_invalid_port_uses_default() {
        let content = "REF001  1.2.3.4  notaport\n";
        let entries = parse_host_file(content, DEXTRA_PORT);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].port, DEXTRA_PORT);
    }

    #[test]
    fn parse_tabs_as_whitespace() {
        let content = "REF001\t1.2.3.4\t30001\n";
        let entries = parse_host_file(content, DEXTRA_PORT);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "REF001");
        assert_eq!(entries[0].address, "1.2.3.4");
        assert_eq!(entries[0].port, 30001);
    }

    #[test]
    fn parse_hostname_addresses() {
        let content = "REF001  reflector.example.com\n";
        let entries = parse_host_file(content, DEXTRA_PORT);

        assert_eq!(entries[0].address, "reflector.example.com");
    }
}
