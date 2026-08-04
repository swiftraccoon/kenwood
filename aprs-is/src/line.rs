//! TNC2-format APRS-IS line parsing and formatting.

use std::fmt::{self, Write as _};

use crate::q_construct::QConstruct;
use crate::{AprsIsUplinkLine, AprsIsUplinkLineError};

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

/// Build one validated APRS packet for transmission to APRS-IS.
///
/// Builds `source>destination,path:data\r\n`, joining path elements with
/// commas. The returned proof type guarantees exactly one trailing `CRLF`, no
/// framing bytes in the body, and the APRS-IS 512-byte line limit.
///
/// **Note:** the APRS-IS server ignores / overwrites the Q-construct
/// element in the path if one isn't present (it adds its own based on
/// how the packet arrived). For explicit Q-construct handling use
/// [`crate::format_is_packet_with_qconstruct`].
/// # Errors
///
/// Returns [`AprsIsUplinkLineError::EmbeddedNewline`] if any input contains a
/// carriage return or line feed, or [`AprsIsUplinkLineError::TooLong`] if the
/// completed wire line exceeds 512 bytes.
pub fn format_is_packet(
    source: &str,
    destination: &str,
    path: &[&str],
    data: &str,
) -> Result<AprsIsUplinkLine, AprsIsUplinkLineError> {
    let mut packet = format!("{source}>{destination}");
    for p in path {
        packet.push(',');
        packet.push_str(p);
    }
    packet.push(':');
    packet.push_str(data);
    AprsIsUplinkLine::from_body_bytes(packet.as_bytes())
}

/// Address-bearing part of an APRS-IS packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsIsAddressField {
    /// Packet source, before `>`.
    Source,
    /// Packet destination, after `>` and before the first comma or colon.
    Destination,
    /// One comma-delimited path element.
    Path,
}

impl fmt::Display for AprsIsAddressField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Source => "source",
            Self::Destination => "destination",
            Self::Path => "path",
        })
    }
}

/// Why bytes could not be parsed as one canonical APRS-IS packet line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AprsIsLineError {
    /// The line body was empty.
    #[error("APRS-IS packet line is empty")]
    Empty,
    /// A server comment was supplied where a packet was required.
    #[error("APRS-IS server comment is not a packet line")]
    Comment,
    /// A carriage return or line feed appeared inside the packet body.
    #[error("APRS-IS packet body contains framing byte {byte:#04X} at byte index {index}")]
    EmbeddedNewline {
        /// Offset within the supplied bytes.
        index: usize,
        /// The invalid `CR` or `LF` byte.
        byte: u8,
    },
    /// A required TNC2 separator was missing.
    #[error("APRS-IS packet is missing the {separator:?} separator")]
    MissingSeparator {
        /// Separator that was required.
        separator: char,
    },
    /// A header field was not ASCII text.
    #[error("APRS-IS {field} contains non-ASCII byte {byte:#04X} at byte index {index}")]
    NonAsciiIdentity {
        /// Header field containing the byte.
        field: AprsIsAddressField,
        /// Offset within that field.
        index: usize,
        /// Invalid byte.
        byte: u8,
    },
    /// A source, destination, or path identity violated APRS-IS text rules.
    #[error("invalid APRS-IS {field} identity {value:?}: {reason}")]
    InvalidIdentity {
        /// Header field containing the invalid identity.
        field: AprsIsAddressField,
        /// Exact rejected field text.
        value: String,
        /// Protocol rule that was violated.
        reason: &'static str,
    },
    /// A comma produced an empty path element.
    #[error("APRS-IS path element {index} is empty")]
    EmptyPathElement {
        /// Zero-based path-element index.
        index: usize,
    },
    /// More than one Q-construct appeared in the path.
    #[error("APRS-IS path contains more than one Q-construct")]
    MultipleQConstructs,
    /// A Q-construct carried an illegal TNC2 repeated marker.
    #[error("APRS-IS Q-construct {value:?} is not canonical")]
    NonCanonicalQConstruct {
        /// Exact rejected path element.
        value: String,
    },
}

/// A validated APRS-IS source, destination, or path identity.
///
/// APRS-IS identities are not limited to RF AX.25 SSIDs. Per the APRS-IS
/// Connecting and Server Design specifications, they may contain a one- or
/// two-character alphanumeric suffix and may be up to nine characters total;
/// server examples include `AE5PL-TS` and `AE5PL-JS`. The exact input case is
/// retained because an APRS-IS header must be forwarded without normalization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AprsIsIdentity(Box<str>);

impl AprsIsIdentity {
    /// Return the exact validated identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the base identity before an optional SSID suffix.
    #[must_use]
    pub fn base(&self) -> &str {
        self.0
            .split_once('-')
            .map_or(self.as_str(), |(base, _)| base)
    }
}

impl fmt::Display for AprsIsIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One non-Q APRS-IS path token, including its optional repeated marker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsIsRouteEntry {
    identity: AprsIsIdentity,
    has_repeated: bool,
}

impl AprsIsRouteEntry {
    /// Path identity without the trailing `*` marker.
    #[must_use]
    pub const fn identity(&self) -> &AprsIsIdentity {
        &self.identity
    }

    /// Whether the TNC2 token ended in the has-been-repeated `*` marker.
    #[must_use]
    pub const fn has_repeated(&self) -> bool {
        self.has_repeated
    }
}

impl fmt::Display for AprsIsRouteEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.identity.fmt(formatter)?;
        if self.has_repeated {
            formatter.write_char('*')?;
        }
        Ok(())
    }
}

/// One validated element in an APRS-IS packet path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AprsIsPathElement {
    /// APRS-IS identity, optionally carrying the TNC2 trailing `*`
    /// has-been-repeated marker.
    Route(AprsIsRouteEntry),
    /// APRS-IS routing metadata. Q-constructs are not AX.25 addresses and
    /// therefore have their own variant.
    QConstruct(QConstruct),
}

impl fmt::Display for AprsIsPathElement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(route) => route.fmt(formatter),
            Self::QConstruct(construct) => construct.fmt(formatter),
        }
    }
}

/// A strictly parsed APRS-IS packet line.
///
/// Source, destination, and every address-bearing path element are stored as
/// APRS-IS identity types. The information field remains byte-native: APRS-IS
/// feeds can carry non-UTF-8 Mic-E and weather bytes, and converting those
/// bytes through a lossy `String` would silently corrupt a later RF frame.
/// Only line-framing bytes (`CR` and `LF`) are forbidden in the information
/// field because they cannot belong to one APRS-IS line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsLine {
    source: AprsIsIdentity,
    destination: AprsIsIdentity,
    path: Vec<AprsIsPathElement>,
    information: Box<[u8]>,
}

impl AprsIsLine {
    /// Parse exact APRS-IS packet bytes.
    ///
    /// The input may omit line framing, end in `LF`, or end in `CRLF`.
    /// After removing that one optional terminator, any remaining `CR` or
    /// `LF` is rejected instead of being copied into another protocol frame.
    /// Header identities must satisfy APRS-IS's alphanumeric, optional-SSID,
    /// and nine-byte rules; this parser never silently normalizes wire input.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsLineError`] for comments, malformed TNC2 structure,
    /// embedded framing bytes, non-ASCII header bytes, invalid APRS-IS
    /// identities, empty path elements, or duplicate Q-constructs.
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self, AprsIsLineError> {
        let supplied = bytes.as_ref();
        let body = supplied
            .strip_suffix(b"\r\n")
            .or_else(|| supplied.strip_suffix(b"\n"))
            .unwrap_or(supplied);

        if body.is_empty() {
            return Err(AprsIsLineError::Empty);
        }
        if body.first() == Some(&b'#') {
            return Err(AprsIsLineError::Comment);
        }
        if let Some((index, &byte)) = body
            .iter()
            .enumerate()
            .find(|(_, byte)| matches!(byte, b'\r' | b'\n'))
        {
            return Err(AprsIsLineError::EmbeddedNewline { index, byte });
        }

        let colon = body
            .iter()
            .position(|&byte| byte == b':')
            .ok_or(AprsIsLineError::MissingSeparator { separator: ':' })?;
        let header = body.get(..colon).unwrap_or_else(|| {
            unreachable!("colon was found at a valid byte index in the packet body")
        });
        let information = body
            .get(colon + 1..)
            .unwrap_or_else(|| unreachable!("colon + 1 is at most the packet-body length"));
        let greater_than = header
            .iter()
            .position(|&byte| byte == b'>')
            .ok_or(AprsIsLineError::MissingSeparator { separator: '>' })?;
        let source_bytes = header.get(..greater_than).unwrap_or_else(|| {
            unreachable!("greater-than separator has a valid index in the packet header")
        });
        let destination_and_path = header.get(greater_than + 1..).unwrap_or_else(|| {
            unreachable!("greater-than index + 1 is at most the packet-header length")
        });

        let (destination_bytes, path_bytes) = destination_and_path
            .iter()
            .position(|&byte| byte == b',')
            .map_or((destination_and_path, None), |comma| {
                let destination = destination_and_path.get(..comma).unwrap_or_else(|| {
                    unreachable!("comma was found at a valid destination byte index")
                });
                let path = destination_and_path.get(comma + 1..).unwrap_or_else(|| {
                    unreachable!("comma index + 1 is at most the header length")
                });
                (destination, Some(path))
            });

        let source = parse_address(source_bytes, AprsIsAddressField::Source)?;
        let destination = parse_address(destination_bytes, AprsIsAddressField::Destination)?;
        let mut path = Vec::new();
        let mut qconstruct = None;

        if let Some(path_bytes) = path_bytes {
            for (index, element) in path_bytes.split(|&byte| byte == b',').enumerate() {
                if element.is_empty() {
                    return Err(AprsIsLineError::EmptyPathElement { index });
                }
                let element = ascii_field(element, AprsIsAddressField::Path)?;
                if let Some(construct) = QConstruct::from_path_element(element) {
                    if element != construct.as_str() {
                        return Err(AprsIsLineError::NonCanonicalQConstruct {
                            value: element.to_owned(),
                        });
                    }
                    if qconstruct.replace(construct).is_some() {
                        return Err(AprsIsLineError::MultipleQConstructs);
                    }
                    path.push(AprsIsPathElement::QConstruct(construct));
                    continue;
                }

                let (address_text, has_repeated) = element
                    .strip_suffix('*')
                    .map_or((element, false), |address| (address, true));
                let identity = parse_identity(address_text.as_bytes(), AprsIsAddressField::Path)?;
                path.push(AprsIsPathElement::Route(AprsIsRouteEntry {
                    identity,
                    has_repeated,
                }));
            }
        }

        Ok(Self {
            source,
            destination,
            path,
            information: information.into(),
        })
    }

    /// Packet source address.
    #[must_use]
    pub const fn source(&self) -> &AprsIsIdentity {
        &self.source
    }

    /// Packet destination address.
    #[must_use]
    pub const fn destination(&self) -> &AprsIsIdentity {
        &self.destination
    }

    /// Validated path elements in original order.
    #[must_use]
    pub fn path(&self) -> &[AprsIsPathElement] {
        &self.path
    }

    /// Exact information-field bytes after the first `:`.
    #[must_use]
    pub fn information(&self) -> &[u8] {
        &self.information
    }

    /// The packet's Q-construct, when present.
    #[must_use]
    pub fn qconstruct(&self) -> Option<QConstruct> {
        self.path.iter().find_map(|element| match element {
            AprsIsPathElement::QConstruct(construct) => Some(*construct),
            AprsIsPathElement::Route(_) => None,
        })
    }

    /// Exact validated TNC2 header, excluding the colon and information field.
    #[must_use]
    pub fn tnc2_header(&self) -> String {
        let mut header = format!("{}>{}", self.source, self.destination);
        for element in &self.path {
            header.push(',');
            write!(&mut header, "{element}")
                .unwrap_or_else(|_| unreachable!("writing to a String cannot fail"));
        }
        header
    }

    /// `true` when the APRS-IS packet must not be gated to RF.
    ///
    /// `NOGATE`, `RFONLY`, and unverified `TCPXX` tokens are matched by base
    /// identity, ignoring an SSID and the separately stored trailing `*`.
    /// Unverified-login `qAX` and non-propagating `qAZ` constructs also block
    /// RF transmission; suffixed route tokens with those bases fail closed as
    /// well. `TCPIP` deliberately does not: APRS-IS requires
    /// client-originated Internet packets to carry `TCPIP*`, so treating it as
    /// an RF opt-out would reject ordinary verified messages. RF-to-IS gating
    /// applies the opposite-direction TCPIP loop check in `igate_rewritten_path`.
    #[must_use]
    pub fn blocks_gating_to_rf(&self) -> bool {
        self.path.iter().any(|element| match element {
            AprsIsPathElement::QConstruct(QConstruct::QAX | QConstruct::QAZ) => true,
            AprsIsPathElement::QConstruct(_) => false,
            AprsIsPathElement::Route(route) => matches_base_ignore_ascii_case(
                route.identity(),
                &["NOGATE", "RFONLY", "TCPXX", "qAX", "qAZ"],
            ),
        })
    }
}

impl TryFrom<&[u8]> for AprsIsLine {
    type Error = AprsIsLineError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(bytes)
    }
}

impl TryFrom<&str> for AprsIsLine {
    type Error = AprsIsLineError;

    fn try_from(line: &str) -> Result<Self, Self::Error> {
        Self::parse(line)
    }
}

fn ascii_field(bytes: &[u8], field: AprsIsAddressField) -> Result<&str, AprsIsLineError> {
    if let Some((index, &byte)) = bytes.iter().enumerate().find(|(_, byte)| !byte.is_ascii()) {
        return Err(AprsIsLineError::NonAsciiIdentity { field, index, byte });
    }
    Ok(std::str::from_utf8(bytes)
        .unwrap_or_else(|_| unreachable!("an all-ASCII byte slice is valid UTF-8")))
}

fn parse_address(
    bytes: &[u8],
    field: AprsIsAddressField,
) -> Result<AprsIsIdentity, AprsIsLineError> {
    parse_identity(bytes, field)
}

fn parse_identity(
    bytes: &[u8],
    field: AprsIsAddressField,
) -> Result<AprsIsIdentity, AprsIsLineError> {
    let value = ascii_field(bytes, field)?;
    let invalid = |reason| AprsIsLineError::InvalidIdentity {
        field,
        value: value.to_owned(),
        reason,
    };

    if value.len() > 9 {
        return Err(invalid("identity exceeds 9 ASCII characters"));
    }
    let (base, suffix) = value
        .split_once('-')
        .map_or((value, None), |(base, suffix)| (base, Some(suffix)));
    let minimum = if field == AprsIsAddressField::Source {
        3
    } else {
        1
    };
    if base.len() < minimum {
        return Err(invalid(if field == AprsIsAddressField::Source {
            "source base must contain at least 3 alphanumeric characters"
        } else {
            "identity base must contain at least 1 alphanumeric character"
        }));
    }
    if !base.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(invalid(
            "base must contain only ASCII alphanumeric characters",
        ));
    }
    if value.matches('-').count() > 1 {
        return Err(invalid("identity may contain only one SSID separator"));
    }
    if let Some(suffix) = suffix {
        if !(1..=2).contains(&suffix.len()) {
            return Err(invalid("SSID must contain 1 or 2 characters"));
        }
        if !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(invalid(
                "SSID must contain only ASCII alphanumeric characters",
            ));
        }
        if suffix == "0" {
            return Err(invalid("SSID zero must be omitted"));
        }
    }

    Ok(AprsIsIdentity(value.into()))
}

fn matches_base_ignore_ascii_case(identity: &AprsIsIdentity, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| identity.base().eq_ignore_ascii_case(candidate))
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
        assert_eq!(
            parse_is_line(line),
            Some("N0CALL>APK005,WIDE1-1:!4903.50N/07201.75W-Test")
        );
    }

    #[test]
    fn format_packet_no_path() -> TestResult {
        let packet = format_is_packet("N0CALL", "APK005", &[], "!4903.50N/07201.75W-Test")?;
        assert_eq!(
            packet.as_bytes(),
            b"N0CALL>APK005:!4903.50N/07201.75W-Test\r\n"
        );
        Ok(())
    }

    #[test]
    fn format_packet_with_path() -> TestResult {
        let packet = format_is_packet(
            "N0CALL",
            "APK005",
            &["WIDE1-1", "qAR", "W1AW"],
            "!4903.50N/07201.75W-Test",
        )?;
        assert_eq!(
            packet.as_bytes(),
            b"N0CALL>APK005,WIDE1-1,qAR,W1AW:!4903.50N/07201.75W-Test\r\n"
        );
        Ok(())
    }

    #[test]
    fn format_packet_rejects_framing_in_every_component() {
        for result in [
            format_is_packet("N0CALL\r\nFORGED", "APK005", &[], "data"),
            format_is_packet("N0CALL", "APK005\nFORGED", &[], "data"),
            format_is_packet("N0CALL", "APK005", &["WIDE1-1\rFORGED"], "data"),
            format_is_packet("N0CALL", "APK005", &[], "data\r\nFORGED>X:x"),
        ] {
            assert!(matches!(
                result,
                Err(AprsIsUplinkLineError::EmbeddedNewline { .. })
            ));
        }
    }

    #[test]
    fn strict_line_parses_typed_fields_and_preserves_information_bytes() -> TestResult {
        let wire = b"N0CALL-7>APK005,WIDE1-1*,qAR,W1AW:`\xC1\x82\r\n";
        let parsed = AprsIsLine::parse(wire)?;

        assert_eq!(parsed.source().to_string(), "N0CALL-7");
        assert_eq!(parsed.destination().to_string(), "APK005");
        assert_eq!(parsed.path().len(), 3);
        assert_eq!(
            parsed.path().first().map(ToString::to_string).as_deref(),
            Some("WIDE1-1*")
        );
        assert_eq!(
            parsed.path().get(1),
            Some(&AprsIsPathElement::QConstruct(QConstruct::QAR))
        );
        assert_eq!(
            parsed.path().get(2).map(ToString::to_string).as_deref(),
            Some("W1AW")
        );
        assert_eq!(parsed.information(), b"`\xC1\x82");
        assert_eq!(parsed.qconstruct(), Some(QConstruct::QAR));
        assert_eq!(parsed.tnc2_header(), "N0CALL-7>APK005,WIDE1-1*,qAR,W1AW");
        Ok(())
    }

    #[test]
    fn strict_line_accepts_no_path_and_empty_information() -> TestResult {
        let parsed = AprsIsLine::parse("N0CALL>APK005:")?;
        assert!(parsed.path().is_empty());
        assert!(parsed.information().is_empty());
        assert_eq!(parsed.qconstruct(), None);
        Ok(())
    }

    #[test]
    fn strict_line_rejects_missing_separators_and_empty_path_elements() {
        assert!(matches!(
            AprsIsLine::parse("no header separator"),
            Err(AprsIsLineError::MissingSeparator { separator: ':' })
        ));
        assert!(matches!(
            AprsIsLine::parse("ONLYDEST:no source separator"),
            Err(AprsIsLineError::MissingSeparator { separator: '>' })
        ));
        assert!(matches!(
            AprsIsLine::parse("N0CALL>APK005,:data"),
            Err(AprsIsLineError::EmptyPathElement { index: 0 })
        ));
    }

    #[test]
    fn strict_line_rejects_crlf_contamination() {
        for contaminated in [
            b"N0CALL>APK005:hello\r\nEVIL>APRS:forged\r\n".as_slice(),
            b"N0CALL>APK005:hello\rworld".as_slice(),
            b"N0CALL>APK005:hello\nworld".as_slice(),
        ] {
            assert!(
                matches!(
                    AprsIsLine::parse(contaminated),
                    Err(AprsIsLineError::EmbeddedNewline { .. })
                ),
                "accepted contaminated line {contaminated:?}"
            );
        }
    }

    #[test]
    fn strict_line_rejects_unicode_and_controls_in_every_identity_field() {
        for invalid in [
            "NØCALL>APK005:data",
            "N0CALL>AP\u{0001}005:data",
            "N0CALL>APK005,WIDE\u{007f}:data",
            "N0CALL>APK005,WIDÉ:data",
        ] {
            assert!(
                AprsIsLine::parse(invalid).is_err(),
                "accepted invalid identity in {invalid:?}"
            );
        }
    }

    #[test]
    fn strict_line_rejects_noncanonical_addresses_and_paths() {
        for invalid in [
            "N0CALL-0>APK005:data",
            "NO>APK005:data",
            "N0CALL>APK005,WIDE1-ABC:data",
            "N0CALL>APK005,WIDE1-1**:data",
            "N0CALL>APK005,qAR*:data",
            "N0CALL>APK005,qAR,qAC:data",
        ] {
            assert!(
                AprsIsLine::parse(invalid).is_err(),
                "accepted noncanonical line {invalid:?}"
            );
        }
    }

    #[test]
    fn strict_line_accepts_aprs_is_alphanumeric_identities_exactly() -> TestResult {
        let parsed = AprsIsLine::parse("AE5PL-TS>aprs,TCPIP*,qAC,AE5PL-JS:data")?;
        assert_eq!(parsed.source().as_str(), "AE5PL-TS");
        assert_eq!(parsed.destination().as_str(), "aprs");
        assert_eq!(parsed.tnc2_header(), "AE5PL-TS>aprs,TCPIP*,qAC,AE5PL-JS");
        assert_eq!(
            parsed.path().get(2).map(ToString::to_string).as_deref(),
            Some("AE5PL-JS")
        );
        Ok(())
    }

    #[test]
    fn no_gate_marker_applies_is_to_rf_security_semantics() -> TestResult {
        for marker in [
            "NOGATE",
            "nogate-1*",
            "RFONLY-AA",
            "TCPXX*",
            "TCPXX-15*",
            "qAX",
            "qAX-1*",
            "qAZ",
        ] {
            let parsed = AprsIsLine::parse(format!("SRC>B,{marker}:data"))?;
            assert!(parsed.blocks_gating_to_rf(), "missed marker {marker}");
        }
        for allowed in ["WIDE1-1", "TCPIP", "TCPIP*", "TCPIP-1*", "qAC"] {
            let parsed = AprsIsLine::parse(format!("SRC>B,{allowed}:data"))?;
            assert!(
                !parsed.blocks_gating_to_rf(),
                "ordinary verified IS marker was blocked: {allowed}"
            );
        }
        Ok(())
    }
}
