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
    pub fn as_wire(self) -> i32 {
        match self {
            Self::Verified(n) => i32::from(n),
            Self::ReceiveOnly => -1,
        }
    }
}

impl std::fmt::Display for Passcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_wire().fmt(f)
    }
}

/// Structured APRS-IS filter expression.
///
/// Per <http://www.aprs-is.net/javAPRSFilter.aspx>, APRS-IS servers
/// accept a small query language for selecting which packets to deliver
/// to a client connection. Each filter is one or more tokens separated
/// by spaces. This enum covers the commonly-used forms; use
/// [`AprsIsFilter::raw`] to drop in any literal filter string for
/// advanced cases.
#[derive(Debug, Clone, PartialEq)]
pub enum AprsIsFilter {
    /// Range filter `r/lat/lon/distance_km` — packets from stations
    /// within the given radius.
    Range {
        /// Centre latitude in degrees (positive = North).
        lat: f64,
        /// Centre longitude in degrees (positive = East).
        lon: f64,
        /// Radius in kilometres.
        distance_km: f64,
    },
    /// Area / box filter `a/lat1/lon1/lat2/lon2` — packets within a
    /// lat/lon bounding box (NW and SE corners).
    Area {
        /// Northwest latitude.
        lat1: f64,
        /// Northwest longitude.
        lon1: f64,
        /// Southeast latitude.
        lat2: f64,
        /// Southeast longitude.
        lon2: f64,
    },
    /// Prefix filter `p/aa/bb/cc` — packets whose source callsign
    /// begins with any of the given prefixes.
    Prefix(Vec<String>),
    /// Budlist filter `b/call1/call2` — packets from exactly these
    /// stations.
    Budlist(Vec<String>),
    /// Object filter `o/obj1/obj2` — object reports with these names.
    Object(Vec<String>),
    /// Type filter `t/poimntqsu` — characters select which frame types
    /// are wanted (p=position, o=object, i=item, m=message, n=nws,
    /// t=telemetry, q=query, s=status, u=user-defined).
    Type(String),
    /// Symbol filter `s/sym1sym2/...` — symbols to include.
    Symbol(String),
    /// "Friend" / range-around-station filter `f/call/distance_km`.
    Friend {
        /// Station to centre on.
        callsign: String,
        /// Distance in km.
        distance_km: f64,
    },
    /// Group message filter `g/name` — bulletins addressed to this
    /// group.
    Group(String),
    /// Raw literal filter string for advanced / uncommon cases.
    Raw(String),
}

impl AprsIsFilter {
    /// Build a raw literal filter expression.
    #[must_use]
    pub fn raw(s: impl Into<String>) -> Self {
        Self::Raw(s.into())
    }

    /// Format this filter as the exact wire-format string APRS-IS
    /// servers expect after the `filter ` keyword in the login line.
    #[must_use]
    pub fn as_wire(&self) -> String {
        match self {
            Self::Range {
                lat,
                lon,
                distance_km,
            } => format!("r/{lat}/{lon}/{distance_km}"),
            Self::Area {
                lat1,
                lon1,
                lat2,
                lon2,
            } => format!("a/{lat1}/{lon1}/{lat2}/{lon2}"),
            Self::Prefix(parts) => format!("p/{}", parts.join("/")),
            Self::Budlist(parts) => format!("b/{}", parts.join("/")),
            Self::Object(parts) => format!("o/{}", parts.join("/")),
            Self::Type(chars) => format!("t/{chars}"),
            Self::Symbol(chars) => format!("s/{chars}"),
            Self::Friend {
                callsign,
                distance_km,
            } => format!("f/{callsign}/{distance_km}"),
            Self::Group(name) => format!("g/{name}"),
            Self::Raw(s) => s.clone(),
        }
    }

    /// Combine multiple filter clauses into a single filter string by
    /// joining with spaces — APRS-IS allows an OR of any number of
    /// clauses in a single `filter` directive.
    #[must_use]
    pub fn join(filters: &[Self]) -> String {
        filters
            .iter()
            .map(Self::as_wire)
            .collect::<Vec<_>>()
            .join(" ")
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
            software_name: "kenwood-thd75".to_owned(),
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
///
/// **Note:** the APRS-IS server ignores / overwrites the Q-construct
/// element in the path if one isn't present (it adds its own based on
/// how the packet arrived). For explicit Q-construct handling use
/// [`format_is_packet_with_qconstruct`].
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

/// APRS-IS Q-construct tag (path identifier that records how a packet
/// entered the APRS-IS network).
///
/// Per <http://www.aprs-is.net/q.aspx>, every packet seen by an APRS-IS
/// server has exactly one Q-construct inserted into its path. Servers
/// that relay packets propagate the construct unchanged; servers that
/// originate packets add one based on the packet's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QConstruct {
    /// `qAC` — client-owned, server verified the login.
    QAC,
    /// `qAX` — client-owned, server did *not* verify the login.
    QAX,
    /// `qAU` — client-owned, received via UDP submit.
    QAU,
    /// `qAo` — server-owned, received from a different server.
    QAo,
    /// `qAO` — server-owned, originated on RF (`IGATE`).
    QAO,
    /// `qAS` — server-owned, received from a peer.
    QAS,
    /// `qAr` — gated from RF with no callsign substitution.
    QAr,
    /// `qAR` — gated from RF by a verified login.
    QAR,
    /// `qAZ` — not gated (server-added as a diagnostic).
    QAZ,
}

impl QConstruct {
    /// Wire form of the construct (the exact 3-character token inserted
    /// into the APRS path).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QAC => "qAC",
            Self::QAX => "qAX",
            Self::QAU => "qAU",
            Self::QAo => "qAo",
            Self::QAO => "qAO",
            Self::QAS => "qAS",
            Self::QAr => "qAr",
            Self::QAR => "qAR",
            Self::QAZ => "qAZ",
        }
    }

    /// Parse a path element as a Q-construct if it matches one of the
    /// well-known forms. Returns `None` otherwise.
    #[must_use]
    pub fn from_path_element(s: &str) -> Option<Self> {
        match s {
            "qAC" => Some(Self::QAC),
            "qAX" => Some(Self::QAX),
            "qAU" => Some(Self::QAU),
            "qAo" => Some(Self::QAo),
            "qAO" => Some(Self::QAO),
            "qAS" => Some(Self::QAS),
            "qAr" => Some(Self::QAr),
            "qAR" => Some(Self::QAR),
            "qAZ" => Some(Self::QAZ),
            _ => None,
        }
    }
}

impl std::fmt::Display for QConstruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Format an APRS-IS packet with an explicit Q-construct.
///
/// Injects the Q-construct just before the gate callsign — the form
/// required for packets originated by a client application. Per the
/// APRS-IS spec, clients add `qAC` or `qAX` depending on whether they
/// authenticated.
#[must_use]
pub fn format_is_packet_with_qconstruct(
    source: &str,
    destination: &str,
    path: &[&str],
    qconstruct: QConstruct,
    gate_callsign: &str,
    data: &str,
) -> String {
    let mut packet = format!("{source}>{destination}");
    for p in path {
        packet.push(',');
        packet.push_str(p);
    }
    packet.push(',');
    packet.push_str(qconstruct.as_str());
    packet.push(',');
    packet.push_str(gate_callsign);
    packet.push(':');
    packet.push_str(data);
    packet.push_str("\r\n");
    packet
}

/// A parsed APRS-IS packet line.
///
/// Wraps the fields of a `source>destination,path:data` line without
/// interpreting the data portion. Use the parsers in [`crate::kiss`] to
/// decode the APRS information field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsLine {
    /// Source callsign as it appears on the wire.
    pub source: String,
    /// Destination callsign (APRS tocall).
    pub destination: String,
    /// Path elements (digipeaters + Q-construct + gate).
    pub path: Vec<String>,
    /// Raw information field (everything after the `:`).
    pub data: String,
    /// Parsed Q-construct if one is present in the path.
    pub qconstruct: Option<QConstruct>,
}

impl AprsIsLine {
    /// Parse an APRS-IS packet line. Returns `None` on malformed input
    /// (missing `>` or `:`).
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let (header, data) = trimmed.split_once(':')?;
        let (source, rest) = header.split_once('>')?;
        let mut parts = rest.split(',');
        let destination = parts.next()?.to_owned();
        let path: Vec<String> = parts.map(str::to_owned).collect();
        let qconstruct = path.iter().find_map(|p| QConstruct::from_path_element(p));
        Some(Self {
            source: source.to_owned(),
            destination,
            path,
            data: data.to_owned(),
            qconstruct,
        })
    }

    /// `true` if any of the path elements is `NOGATE`, `RFONLY`,
    /// `TCPIP`, or `TCPXX` (case-insensitive).
    #[must_use]
    pub fn has_no_gate_marker(&self) -> bool {
        self.path.iter().any(|p| {
            let upper = p.to_ascii_uppercase();
            matches!(upper.as_str(), "NOGATE" | "RFONLY" | "TCPIP" | "TCPXX")
        })
    }
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
        assert_eq!(config.passcode, Passcode::Verified(13023));
        assert_eq!(config.server, "rotate.aprs2.net");
        assert_eq!(config.port, 14580);
        assert!(config.filter.is_empty());
        assert_eq!(config.software_name, "kenwood-thd75");
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
    fn qconstruct_round_trip() {
        let all = [
            QConstruct::QAC,
            QConstruct::QAX,
            QConstruct::QAU,
            QConstruct::QAo,
            QConstruct::QAO,
            QConstruct::QAS,
            QConstruct::QAr,
            QConstruct::QAR,
            QConstruct::QAZ,
        ];
        for q in all {
            assert_eq!(QConstruct::from_path_element(q.as_str()), Some(q));
        }
        assert_eq!(QConstruct::from_path_element("WIDE1-1"), None);
    }

    #[test]
    fn format_is_packet_with_qconstruct_injects_tag() {
        let pkt = format_is_packet_with_qconstruct(
            "N0CALL",
            "APK005",
            &["WIDE1-1"],
            QConstruct::QAC,
            "N0CALL",
            "!4903.50N/07201.75W-",
        );
        assert_eq!(
            pkt,
            "N0CALL>APK005,WIDE1-1,qAC,N0CALL:!4903.50N/07201.75W-\r\n"
        );
    }

    #[test]
    fn aprs_is_line_parse_basic() {
        let line = "N0CALL>APK005,WIDE1-1,qAR,W1AW:!4903.50N/07201.75W-Test\r\n";
        let parsed = AprsIsLine::parse(line).unwrap();
        assert_eq!(parsed.source, "N0CALL");
        assert_eq!(parsed.destination, "APK005");
        assert_eq!(parsed.path, vec!["WIDE1-1", "qAR", "W1AW"]);
        assert_eq!(parsed.data, "!4903.50N/07201.75W-Test");
        assert_eq!(parsed.qconstruct, Some(QConstruct::QAR));
    }

    #[test]
    fn aprs_is_line_parse_no_path() {
        let line = "N0CALL>APK005:!test\r\n";
        let parsed = AprsIsLine::parse(line).unwrap();
        assert!(parsed.path.is_empty());
        assert_eq!(parsed.qconstruct, None);
    }

    #[test]
    fn aprs_is_line_parse_malformed_returns_none() {
        assert!(AprsIsLine::parse("no header separator").is_none());
        assert!(AprsIsLine::parse("only>destination no data").is_none());
    }

    #[test]
    fn aprs_is_filter_range_wire_format() {
        let f = AprsIsFilter::Range {
            lat: 35.25,
            lon: -97.75,
            distance_km: 100.0,
        };
        assert_eq!(f.as_wire(), "r/35.25/-97.75/100");
    }

    #[test]
    fn aprs_is_filter_type_and_prefix() {
        let f = AprsIsFilter::Type("po".to_owned());
        assert_eq!(f.as_wire(), "t/po");
        let f = AprsIsFilter::Prefix(vec!["KK".to_owned(), "W1".to_owned()]);
        assert_eq!(f.as_wire(), "p/KK/W1");
    }

    #[test]
    fn aprs_is_filter_join_multiple() {
        let filters = vec![
            AprsIsFilter::Range {
                lat: 35.0,
                lon: -97.0,
                distance_km: 50.0,
            },
            AprsIsFilter::Type("p".to_owned()),
        ];
        let joined = AprsIsFilter::join(&filters);
        assert!(joined.contains("r/35"));
        assert!(joined.contains("t/p"));
        assert!(joined.contains(' '));
    }

    #[test]
    fn aprs_is_filter_raw_passthrough() {
        let f = AprsIsFilter::raw("m/50");
        assert_eq!(f.as_wire(), "m/50");
    }

    #[test]
    fn aprs_is_line_no_gate_marker_detection() {
        let line = AprsIsLine::parse("A>B,NOGATE:data").unwrap();
        assert!(line.has_no_gate_marker());
        let line = AprsIsLine::parse("A>B,WIDE1-1:data").unwrap();
        assert!(!line.has_no_gate_marker());
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
