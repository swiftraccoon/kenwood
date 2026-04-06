//! APRS-IS (Internet Service) client types and helpers for `IGate` and
//! network operations.
//!
//! APRS-IS is a TCP-based network that connects APRS clients worldwide.
//! This module provides the configuration types, passcode computation,
//! and packet formatting helpers. The actual TCP connection is left to
//! the caller (bring your own transport).
//!
//! # APRS-IS Protocol
//!
//! - TCP connection to server (e.g., `rotate.aprs2.net:14580`)
//! - Login: `user CALL pass PASSCODE vers SOFTNAME SOFTVER filter FILTER\r\n`
//! - Packets are ASCII lines terminated by `\r\n`
//! - Server sends `# comment` lines for keepalive/info
//! - Client should send keepalive every 2 minutes if idle
//!
//! # Passcode
//!
//! The APRS-IS passcode is computed from the callsign (without SSID)
//! using a well-known hash algorithm. Use [`aprs_is_passcode`] to compute it.

/// APRS-IS (Internet Service) client configuration.
///
/// Connects to an APRS-IS server via TCP, authenticates with callsign
/// and passcode, and allows sending/receiving APRS packets over the
/// internet backbone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsConfig {
    /// Callsign with optional SSID (e.g., "N0CALL-10").
    pub callsign: String,
    /// APRS-IS passcode (computed from callsign, or -1 for receive-only).
    pub passcode: i32,
    /// Server hostname (e.g., "rotate.aprs2.net").
    pub server: String,
    /// Server port (default 14580).
    pub port: u16,
    /// APRS-IS filter string (e.g., "r/35.25/-97.75/100" for 100km radius).
    pub filter: String,
    /// Software name for login.
    pub software_name: String,
    /// Software version for login.
    pub software_version: String,
}

impl AprsIsConfig {
    /// Create a new APRS-IS configuration with sensible defaults.
    ///
    /// Computes the passcode automatically from the callsign. Defaults to
    /// `rotate.aprs2.net:14580` with no filter.
    #[must_use]
    pub fn new(callsign: &str) -> Self {
        let passcode = aprs_is_passcode(callsign);
        Self {
            callsign: callsign.to_owned(),
            passcode,
            server: "rotate.aprs2.net".to_owned(),
            port: 14580,
            filter: String::new(),
            software_name: "kenwood-thd75".to_owned(),
            software_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Compute the APRS-IS passcode from a callsign.
///
/// The algorithm is a simple hash of the callsign characters (without SSID).
/// This is NOT cryptographic -- it is a well-known public algorithm used
/// by all APRS software.
///
/// # Algorithm
///
/// 1. Strip SSID (everything from `-` onward).
/// 2. Uppercase the base callsign.
/// 3. Starting with hash = 0x73E2, XOR each pair of bytes: first byte
///    shifted left 8 bits, second byte as-is. If the callsign has an odd
///    number of characters, the last byte is XOR'd shifted left 8 bits.
/// 4. Mask with 0x7FFF to produce a positive 15-bit value.
#[must_use]
pub fn aprs_is_passcode(callsign: &str) -> i32 {
    // Strip SSID.
    let base = callsign
        .split('-')
        .next()
        .unwrap_or(callsign)
        .to_uppercase();

    let bytes = base.as_bytes();
    let mut hash: u16 = 0x73E2;

    let mut i = 0;
    while i < bytes.len() {
        hash ^= u16::from(bytes[i]) << 8;
        if i + 1 < bytes.len() {
            hash ^= u16::from(bytes[i + 1]);
        }
        i += 2;
    }

    i32::from(hash & 0x7FFF)
}

/// Build the APRS-IS login string.
///
/// Format: `user CALL pass PASSCODE vers SOFTNAME SOFTVER filter FILTER\r\n`
///
/// If the filter is empty, the `filter` clause is omitted.
#[must_use]
pub fn build_login_string(config: &AprsIsConfig) -> String {
    let mut login = format!(
        "user {} pass {} vers {} {}",
        config.callsign, config.passcode, config.software_name, config.software_version,
    );

    if !config.filter.is_empty() {
        login.push_str(" filter ");
        login.push_str(&config.filter);
    }

    login.push_str("\r\n");
    login
}

/// Parse an APRS-IS server line.
///
/// Returns `None` for comment/keepalive lines (starting with `#`),
/// `Some(packet_str)` for APRS packet lines.
#[must_use]
pub fn parse_is_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed.starts_with('#') {
        None
    } else {
        Some(trimmed)
    }
}

/// Format an APRS packet for transmission to APRS-IS.
///
/// Builds the `source>destination,path:data\r\n` string. The path
/// elements are joined with commas.
#[must_use]
pub fn format_is_packet(source: &str, destination: &str, path: &[&str], data: &str) -> String {
    let mut packet = format!("{source}>{destination}");
    for p in path {
        packet.push(',');
        packet.push_str(p);
    }
    packet.push(':');
    packet.push_str(data);
    packet.push_str("\r\n");
    packet
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passcode_n0call() {
        // Well-known test vector: N0CALL -> 13023
        assert_eq!(aprs_is_passcode("N0CALL"), 13023);
    }

    #[test]
    fn passcode_strips_ssid() {
        assert_eq!(aprs_is_passcode("N0CALL-10"), aprs_is_passcode("N0CALL"));
    }

    #[test]
    fn passcode_case_insensitive() {
        assert_eq!(aprs_is_passcode("n0call"), aprs_is_passcode("N0CALL"));
    }

    #[test]
    fn passcode_is_positive() {
        // The result must always be a positive 15-bit value.
        let code = aprs_is_passcode("W1AW");
        assert!((0..=0x7FFF).contains(&code));
    }

    #[test]
    fn passcode_odd_length_callsign() {
        // 5-character callsign (odd length).
        let code = aprs_is_passcode("K1ABC");
        assert!((0..=0x7FFF).contains(&code));
    }

    #[test]
    fn passcode_w1aw() {
        // W1AW -> computed via standard APRS-IS algorithm.
        assert_eq!(aprs_is_passcode("W1AW"), 25988);
    }

    #[test]
    fn config_defaults() {
        let config = AprsIsConfig::new("N0CALL-10");
        assert_eq!(config.callsign, "N0CALL-10");
        assert_eq!(config.passcode, 13023);
        assert_eq!(config.server, "rotate.aprs2.net");
        assert_eq!(config.port, 14580);
        assert!(config.filter.is_empty());
        assert_eq!(config.software_name, "kenwood-thd75");
    }

    #[test]
    fn login_string_no_filter() {
        let config = AprsIsConfig::new("N0CALL");
        let login = build_login_string(&config);
        assert!(login.starts_with("user N0CALL pass 13023 vers kenwood-thd75 "));
        assert!(login.ends_with("\r\n"));
        assert!(!login.contains("filter"));
    }

    #[test]
    fn login_string_with_filter() {
        let mut config = AprsIsConfig::new("N0CALL");
        config.filter = "r/35.25/-97.75/100".to_owned();
        let login = build_login_string(&config);
        assert!(login.contains("filter r/35.25/-97.75/100"));
        assert!(login.ends_with("\r\n"));
    }

    #[test]
    fn parse_comment_line() {
        assert_eq!(parse_is_line("# javAPRSSrvr 4.2.0b05"), None);
    }

    #[test]
    fn parse_empty_line() {
        assert_eq!(parse_is_line(""), None);
        assert_eq!(parse_is_line("\r\n"), None);
    }

    #[test]
    fn parse_packet_line() {
        let line = "N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test\r\n";
        let result = parse_is_line(line);
        assert_eq!(
            result,
            Some("N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test")
        );
    }

    #[test]
    fn format_packet_no_path() {
        let pkt = format_is_packet("N0CALL", "APK005", &[], "!4903.50N/07201.75W-Test");
        assert_eq!(pkt, "N0CALL>APK005:!4903.50N/07201.75W-Test\r\n");
    }

    #[test]
    fn format_packet_with_path() {
        let pkt = format_is_packet(
            "N0CALL",
            "APK005",
            &["WIDE1-1", "qAR", "W1AW"],
            "!4903.50N/07201.75W-Test",
        );
        assert_eq!(
            pkt,
            "N0CALL>APK005,WIDE1-1,qAR,W1AW:!4903.50N/07201.75W-Test\r\n"
        );
    }
}
