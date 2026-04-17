//! TNC2-format APRS-IS line parsing and formatting.

use crate::q_construct::QConstruct;

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
/// [`crate::format_is_packet_with_qconstruct`].
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

/// A parsed APRS-IS packet line.
///
/// Wraps the fields of a `source>destination,path:data` line without
/// interpreting the data portion. Use the parsers in the `aprs` crate
/// to decode the APRS information field.
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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

    #[test]
    fn aprs_is_line_parse_basic() -> TestResult {
        let line = "N0CALL>APK005,WIDE1-1,qAR,W1AW:!4903.50N/07201.75W-Test\r\n";
        let parsed = AprsIsLine::parse(line).ok_or("parse failed")?;
        assert_eq!(parsed.source, "N0CALL");
        assert_eq!(parsed.destination, "APK005");
        assert_eq!(parsed.path, vec!["WIDE1-1", "qAR", "W1AW"]);
        assert_eq!(parsed.data, "!4903.50N/07201.75W-Test");
        assert_eq!(parsed.qconstruct, Some(QConstruct::QAR));
        Ok(())
    }

    #[test]
    fn aprs_is_line_parse_no_path() -> TestResult {
        let line = "N0CALL>APK005:!test\r\n";
        let parsed = AprsIsLine::parse(line).ok_or("parse failed")?;
        assert!(parsed.path.is_empty());
        assert_eq!(parsed.qconstruct, None);
        Ok(())
    }

    #[test]
    fn aprs_is_line_parse_malformed_returns_none() {
        assert!(AprsIsLine::parse("no header separator").is_none());
        assert!(AprsIsLine::parse("only>destination no data").is_none());
    }

    #[test]
    fn aprs_is_line_no_gate_marker_detection() -> TestResult {
        let line = AprsIsLine::parse("A>B,NOGATE:data").ok_or("parse failed")?;
        assert!(line.has_no_gate_marker());
        let line = AprsIsLine::parse("A>B,WIDE1-1:data").ok_or("parse failed")?;
        assert!(!line.has_no_gate_marker());
        Ok(())
    }
}
