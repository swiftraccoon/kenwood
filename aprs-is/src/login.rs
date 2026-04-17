//! APRS-IS login string + passcode computation.
//!
//! # Passcode
//!
//! The APRS-IS passcode is computed from the callsign (without SSID)
//! using a well-known hash algorithm. Use [`aprs_is_passcode`] to
//! compute it.

/// APRS-IS authentication passcode.
///
/// Per the APRS-IS authentication spec, a valid passcode is a 15-bit
/// positive integer computed from the callsign via the public
/// [`aprs_is_passcode`] hash. Clients that don't hold a verified passcode
/// (e.g. read-only monitors) may authenticate as "receive-only" which
/// maps to the wire value `-1`.
///
/// This enum replaces the previous `i32` field that used `-1` as a magic
/// sentinel, making illegal states unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Passcode {
    /// A verified 15-bit passcode computed from the station callsign.
    Verified(u16),
    /// Receive-only connection — the server will accept incoming packets
    /// from us but will not forward any packets we transmit to RF.
    ReceiveOnly,
}

impl Passcode {
    /// Compute the passcode for a callsign using the public APRS-IS
    /// hash algorithm.
    #[must_use]
    pub fn for_callsign(callsign: &str) -> Self {
        let value = aprs_is_passcode(callsign);
        // `aprs_is_passcode` masks with 0x7FFF so the result fits in u16.
        Self::Verified(u16::try_from(value).unwrap_or(0))
    }

    /// Convert to the wire representation used in the APRS-IS login
    /// string. `Verified(n)` → `n`, `ReceiveOnly` → `-1`.
    #[must_use]
    pub const fn as_wire(self) -> i32 {
        match self {
            Self::Verified(n) => n as i32,
            Self::ReceiveOnly => -1,
        }
    }
}

impl std::fmt::Display for Passcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_wire().fmt(f)
    }
}

/// APRS-IS (Internet Service) client configuration.
///
/// Connects to an APRS-IS server via TCP, authenticates with callsign
/// and passcode, and allows sending/receiving APRS packets over the
/// internet backbone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsConfig {
    /// Callsign with optional SSID (e.g., "N0CALL-10").
    pub callsign: String,
    /// APRS-IS passcode (computed from callsign, or `ReceiveOnly`).
    pub passcode: Passcode,
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
        Self {
            callsign: callsign.to_owned(),
            passcode: Passcode::for_callsign(callsign),
            server: "rotate.aprs2.net".to_owned(),
            port: 14580,
            filter: String::new(),
            software_name: "aprs-is".to_owned(),
            software_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Create a receive-only APRS-IS configuration for the given
    /// callsign. The server will not forward our transmissions to RF.
    #[must_use]
    pub fn receive_only(callsign: &str) -> Self {
        Self {
            callsign: callsign.to_owned(),
            passcode: Passcode::ReceiveOnly,
            server: "rotate.aprs2.net".to_owned(),
            port: 14580,
            filter: String::new(),
            software_name: "aprs-is".to_owned(),
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
        let Some(first) = bytes.get(i) else {
            break;
        };
        hash ^= u16::from(*first) << 8;
        if let Some(second) = bytes.get(i + 1) {
            hash ^= u16::from(*second);
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
        assert!(
            (0..=0x7FFF).contains(&code),
            "passcode out of 15-bit range: {code}"
        );
    }

    #[test]
    fn passcode_odd_length_callsign() {
        // 5-character callsign (odd length).
        let code = aprs_is_passcode("K1ABC");
        assert!(
            (0..=0x7FFF).contains(&code),
            "passcode out of 15-bit range: {code}"
        );
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
        assert_eq!(config.passcode, Passcode::Verified(13023));
        assert_eq!(config.server, "rotate.aprs2.net");
        assert_eq!(config.port, 14580);
        assert!(config.filter.is_empty());
        assert_eq!(config.software_name, "aprs-is");
    }

    #[test]
    fn receive_only_config() {
        let config = AprsIsConfig::receive_only("N0CALL");
        assert_eq!(config.passcode, Passcode::ReceiveOnly);
        assert_eq!(config.passcode.as_wire(), -1);
    }

    #[test]
    fn passcode_for_callsign_matches_hash() {
        assert_eq!(Passcode::for_callsign("N0CALL"), Passcode::Verified(13023));
        assert_eq!(Passcode::for_callsign("W1AW"), Passcode::Verified(25988));
    }

    #[test]
    fn passcode_receive_only_as_wire() {
        assert_eq!(Passcode::ReceiveOnly.as_wire(), -1);
        assert_eq!(Passcode::Verified(13023).as_wire(), 13023);
    }

    #[test]
    fn login_string_no_filter() {
        let config = AprsIsConfig::new("N0CALL");
        let login = build_login_string(&config);
        assert!(
            login.starts_with("user N0CALL pass 13023 vers aprs-is "),
            "unexpected login prefix: {login:?}"
        );
        assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
        assert!(
            !login.contains("filter"),
            "filter clause unexpectedly present: {login:?}"
        );
    }

    #[test]
    fn login_string_with_filter() {
        let mut config = AprsIsConfig::new("N0CALL");
        config.filter = "r/35.25/-97.75/100".to_owned();
        let login = build_login_string(&config);
        assert!(
            login.contains("filter r/35.25/-97.75/100"),
            "filter not in login string: {login:?}"
        );
        assert!(login.ends_with("\r\n"), "missing CRLF: {login:?}");
    }
}
