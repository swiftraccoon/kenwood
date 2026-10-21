//! APRS status report parsing (APRS 1.0.1 §16 pp.80-82).
//!
//! Per the spec, a status report begins with the `>` data-type
//! identifier and carries one of four sub-formats:
//!
//! 1. **Plain text**: `>FREE_TEXT` (up to 62 chars).
//! 2. **With timestamp**: `>DDHHMMzFREE_TEXT` (DHM Zulu + 55 chars).
//! 3. **With Maidenhead grid + symbol**: `>IO91SX/G` (4 or 6 char grid
//!    locator + `/` + symbol code, optionally followed by a comment).
//! 4. **With beam heading and ERP**: a three-byte `^HP` suffix. `H` encodes
//!    heading in ten-degree steps (`0`-`9`, then `A`-`Z`); `P` is the power
//!    code from the table on p.81. This suffix remains in the status text.
//!
//! The parser separates recognized timestamp and grid prefixes and retains
//! the trailing text, including an uninterpreted beam-heading/ERP suffix.

use std::fmt;

use crate::error::AprsError;
use crate::packet::{AprsReportTimestamp, AprsReportTimestampFormat, MaidenheadGrid};
use crate::text::decode_wire_ascii;
use crate::{AprsSymbol, StatusText, SymbolTable, TimestampedStatusText};

/// A validated APRS status timestamp in day/hour/minute UTC form.
///
/// APRS status reports permit only the seven-byte `DDHHMMz` timestamp.
/// Position and object reports also permit local DHM and UTC HMS forms, so
/// their broader [`AprsReportTimestamp`] type cannot represent this field
/// without allowing invalid status states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsStatusTimestamp(AprsReportTimestamp);

impl AprsStatusTimestamp {
    /// Create a day/hour/minute UTC status timestamp (`DDHHMMz`).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] unless `day` is 1-31,
    /// `hour` is 0-23, and `minute` is 0-59.
    pub fn day_hour_minute_utc(day: u8, hour: u8, minute: u8) -> Result<Self, AprsError> {
        AprsReportTimestamp::day_hour_minute_utc(day, hour, minute).map(Self)
    }

    /// Parse an exact seven-byte `DDHHMMz` status timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] for malformed or out-of-range
    /// fields and for the local-DHM or UTC-HMS formats that status reports do
    /// not permit.
    pub fn from_wire(wire: &str) -> Result<Self, AprsError> {
        Self::try_from(AprsReportTimestamp::from_wire(wire)?)
    }

    /// Format this timestamp as its exact seven-byte wire representation.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for AprsStatusTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl core::str::FromStr for AprsStatusTimestamp {
    type Err = AprsError;

    fn from_str(wire: &str) -> Result<Self, Self::Err> {
        Self::from_wire(wire)
    }
}

impl From<AprsStatusTimestamp> for AprsReportTimestamp {
    fn from(timestamp: AprsStatusTimestamp) -> Self {
        timestamp.0
    }
}

impl TryFrom<AprsReportTimestamp> for AprsStatusTimestamp {
    type Error = AprsError;

    fn try_from(timestamp: AprsReportTimestamp) -> Result<Self, Self::Error> {
        if timestamp.format() == AprsReportTimestampFormat::DayHourMinuteUtc {
            Ok(Self(timestamp))
        } else {
            Err(AprsError::InvalidTimestamp(
                crate::TimestampError::StatusFormat,
            ))
        }
    }
}

/// An APRS status report (data type `>`).
///
/// Separates a recognized timestamp or grid/symbol prefix from the trailing
/// text. Beam-heading and ERP suffixes remain part of that text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AprsStatus {
    /// Status text (trailing free-form portion, structured prefix
    /// stripped). When the status carries no structured prefix this
    /// is the entire body.
    text: String,
    /// Optional 7-byte DHM-Zulu timestamp prefix (`DDHHMMz`). Per APRS 1.0.1
    /// §16 p.80 a status report's timestamp can only be day/hour/minute UTC.
    /// `None` when the
    /// body did not begin with a well-formed DHM-Zulu timestamp (an HMS or
    /// DHM-local 7-byte prefix is treated as status text, not a
    /// timestamp).
    timestamp: Option<AprsStatusTimestamp>,
    /// Optional Maidenhead grid locator prefix (4 or 6 ASCII chars
    /// per APRS 1.0.1 §6 grid-square rules).
    grid_locator: Option<MaidenheadGrid>,
    /// Optional 2-byte symbol pair following a Maidenhead grid prefix.
    /// First byte is the symbol-table character (`/` primary, `\\`
    /// alternate); second byte is the symbol code.
    symbol: Option<AprsSymbol>,
}

impl AprsStatus {
    /// Creates a plain status from checked text.
    #[must_use]
    pub fn new(text: StatusText) -> Self {
        Self {
            text: text.into(),
            timestamp: None,
            grid_locator: None,
            symbol: None,
        }
    }
    /// Creates a DHM-UTC status with its checked 55-byte text.
    #[must_use]
    pub fn timestamped(timestamp: AprsStatusTimestamp, text: TimestampedStatusText) -> Self {
        Self {
            text: text.into(),
            timestamp: Some(timestamp),
            grid_locator: None,
            symbol: None,
        }
    }
    /// Borrows text after any recognized structured prefix.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Returns the DHM-UTC observation timestamp when present.
    #[must_use]
    pub const fn timestamp(&self) -> Option<AprsStatusTimestamp> {
        self.timestamp
    }
    /// Borrows the case-preserved grid prefix when present.
    #[must_use]
    pub const fn grid_locator(&self) -> Option<&MaidenheadGrid> {
        self.grid_locator.as_ref()
    }
    /// Returns the checked symbol following a grid prefix.
    #[must_use]
    pub const fn symbol(&self) -> Option<AprsSymbol> {
        self.symbol
    }
    /// Decodes plain, timestamped or grid-prefixed status text.
    /// Receive compatibility retains overlong printable bodies and unrecognized
    /// timestamp-like prefixes as text; send-side text types enforce their limits.
    ///
    /// # Errors
    /// Returns [`AprsError::InvalidFormat`] without `>` or for control bytes,
    /// and [`AprsError::InvalidTextByte`] for non-ASCII input.
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_status(info)
    }
}

/// Maximum status text length per APRS 1.0.1 §16 p.80.
///
/// The spec quote is "up to 62 characters". With a 7-byte timestamp
/// the spec allows 55 characters; with a 6-char Maidenhead+symbol
/// prefix it allows 54. Receive compatibility retains overlong text in an
/// immutable observation; outgoing text types enforce the corresponding limits.
pub const MAX_APRS_STATUS_TEXT_LEN: usize = 62;

/// Parse an APRS status report (`>text`).
///
/// Recognises the four spec-defined sub-formats described in
/// [`AprsStatus`]. The dispatch order is:
///
/// 1. If the body starts with a 7-byte DHM-Zulu timestamp
///    (`DDHHMMz`, the only timestamp form a status report may carry,
///    APRS 1.0.1 §16 p.80), strip the prefix and surface it in the
///    `timestamp` field. A 7-byte HMS or DHM-local prefix is *not* a
///    valid status timestamp and is left as status text.
/// 2. Otherwise, if the body starts with a 6- or 4-char Maidenhead
///    grid locator followed by `/` and one byte (symbol code), strip
///    the grid+symbol prefix and surface them.
/// 3. The remainder becomes `text` (trimmed of trailing whitespace).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not
/// begin with `>`.
pub(crate) fn parse_aprs_status(info: &[u8]) -> Result<AprsStatus, AprsError> {
    if info.first() != Some(&b'>') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).unwrap_or(&[]);
    let body_str = decode_wire_ascii(crate::AprsWireTextField::StatusBody, body)?;
    let body_str = body_str.trim_end_matches(['\r', '\n']);
    if body_str.bytes().any(|byte| !(b' '..=b'~').contains(&byte)) {
        return Err(AprsError::InvalidFormat);
    }

    // Sub-format 2: DHM-Zulu timestamp prefix (7 bytes).
    //
    // APRS 1.0.1 §16 p.80: "The timestamp can only be in DHM zulu format."
    // Unlike position/object reports, a status report MUST NOT carry an
    // HMS (`HHMMSSh`) or DHM-local (`DDHHMM/`) timestamp. The broader report
    // timestamp type accepts those other forms, so a free-text status like
    // ">120000hrs since reset" would otherwise mis-parse "120000h" as an
    // HMS timestamp and silently truncate the text to "rs since reset".
    // Only strip the prefix when it is the spec-legal DHM-UTC format;
    // any other parse result means the leading bytes are status text.
    if let Some(prefix) = body_str.get(..7)
        && let Ok(ts) = AprsStatusTimestamp::from_wire(prefix)
    {
        let rest = body_str.get(7..).unwrap_or("").trim_end().to_owned();
        return Ok(AprsStatus {
            text: rest,
            timestamp: Some(ts),
            grid_locator: None,
            symbol: None,
        });
    }

    // Sub-format 3: Maidenhead grid prefix followed by `/symbol`.
    //
    // Spec at §16 p.81: a 6-char grid locator (or 4-char + 2-char
    // trailing extension) followed by `/` and a symbol code, optionally
    // a space + free-form comment text. Detection: 6 or 4 chars of
    // valid grid format, then `/`, then exactly one byte.
    for grid_len in [6usize, 4usize] {
        if let Some(grid_candidate) = body_str.get(..grid_len)
            && let Ok(grid) = MaidenheadGrid::new(grid_candidate)
            && body_str.as_bytes().get(grid_len) == Some(&b'/')
            && let Some(sym_byte) = body_str.as_bytes().get(grid_len + 1).copied()
            && let Ok(symbol) = AprsSymbol::new(SymbolTable::Primary, sym_byte)
        {
            // The symbol-table is implicit `/` (primary) per APRS 1.0.1
            // p.81 example "IO91SX/G": the `/` here is the table marker
            // and `G` is the code.
            let rest = body_str
                .get(grid_len + 2..)
                .unwrap_or("")
                .trim_end()
                .to_owned();
            return Ok(AprsStatus {
                text: rest,
                timestamp: None,
                grid_locator: Some(grid),
                symbol: Some(symbol),
            });
        }
    }

    // Sub-format 1 (fallback): plain text.
    Ok(AprsStatus {
        text: body_str.trim_end().to_owned(),
        timestamp: None,
        grid_locator: None,
        symbol: None,
    })
}

/// Validate an APRS Maidenhead grid locator per APRS 1.0.1 §6 p.25.
///
/// Form: 2 letters (field, `A..R` case-insensitive) + 2 digits
/// (square) + optionally 2 letters (sub-square, `a..x` case-
/// insensitive). Both 4-char and 6-char forms are accepted.
#[cfg(test)]
fn is_valid_grid_locator(s: &str) -> bool {
    MaidenheadGrid::new(s).is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_status_basic() -> TestResult {
        let info = b">Operating on 144.390";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.text, "Operating on 144.390");
        assert_eq!(status.timestamp, None);
        assert_eq!(status.grid_locator, None);
        assert_eq!(status.symbol, None);
        Ok(())
    }

    #[test]
    fn parse_status_empty() -> TestResult {
        let info = b">";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.text, "");
        Ok(())
    }

    #[test]
    fn parse_status_rejects_non_ascii_body_without_replacement() {
        assert_eq!(
            parse_aprs_status(b">ok\xFF"),
            Err(AprsError::InvalidTextByte {
                field: crate::AprsWireTextField::StatusBody,
                index: 2,
                byte: 0xFF,
            }),
        );
    }

    #[test]
    fn parse_status_with_dhm_zulu_timestamp() -> TestResult {
        // Sub-format 2: DHM-Zulu timestamp followed by free text.
        let info = b">092345zOn the air";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.timestamp,
            Some(AprsStatusTimestamp::day_hour_minute_utc(9, 23, 45)?),
        );
        assert_eq!(status.text, "On the air");
        Ok(())
    }

    #[test]
    fn parse_status_rejects_hms_timestamp() -> TestResult {
        // APRS 1.0.1 §16 p.80: a status timestamp can ONLY be DHM-Zulu.
        // A 7-byte HMS prefix (`234517h`) must be treated as status text,
        // not stripped as a timestamp.
        let info = b">234517hLate-night net";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "234517hLate-night net");
        Ok(())
    }

    #[test]
    fn parse_status_rejects_dhm_local_timestamp() -> TestResult {
        // DHM-local (`DDHHMM/`) is likewise not a valid status timestamp.
        let info = b">110000/Local time status";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "110000/Local time status");
        Ok(())
    }

    #[test]
    fn parse_status_hms_lookalike_freetext_not_truncated() -> TestResult {
        // An HMS-like prefix remains part of the complete status text.
        let info = b">120000hrs since reset";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.timestamp, None);
        assert_eq!(status.text, "120000hrs since reset");
        Ok(())
    }

    #[test]
    fn parse_status_dhm_zulu_still_stripped() -> TestResult {
        // The one legal form must still be recognised and stripped.
        let info = b">092345zReal status";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.timestamp,
            Some(AprsStatusTimestamp::day_hour_minute_utc(9, 23, 45)?),
        );
        assert_eq!(status.text, "Real status");
        Ok(())
    }

    #[test]
    fn status_timestamp_round_trips_dhm_utc() -> TestResult {
        let timestamp = AprsStatusTimestamp::day_hour_minute_utc(9, 23, 45)?;

        assert_eq!(timestamp.to_wire_string(), "092345z");
        assert_eq!(AprsStatusTimestamp::from_wire("092345z")?, timestamp);
        assert_eq!("092345z".parse::<AprsStatusTimestamp>()?, timestamp);
        assert_eq!(
            AprsReportTimestamp::from(timestamp),
            AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?,
        );
        Ok(())
    }

    #[test]
    fn status_timestamp_rejects_other_report_formats() -> TestResult {
        let local = AprsReportTimestamp::day_hour_minute_local(11, 0, 0)?;
        let hms = AprsReportTimestamp::hour_minute_second_utc(23, 45, 17)?;

        assert!(AprsStatusTimestamp::try_from(local).is_err());
        assert!(AprsStatusTimestamp::try_from(hms).is_err());
        assert!(AprsStatusTimestamp::from_wire("110000/").is_err());
        assert!(AprsStatusTimestamp::from_wire("234517h").is_err());
        assert!(AprsStatusTimestamp::from_wire("01011234").is_err());
        Ok(())
    }

    #[test]
    fn status_timestamp_rejects_malformed_and_out_of_range_values() {
        assert!(AprsStatusTimestamp::from_wire("092345X").is_err());
        assert!(AprsStatusTimestamp::from_wire("09A345z").is_err());
        assert!(AprsStatusTimestamp::day_hour_minute_utc(0, 0, 0).is_err());
        assert!(AprsStatusTimestamp::day_hour_minute_utc(9, 24, 0).is_err());
        assert!(AprsStatusTimestamp::day_hour_minute_utc(9, 23, 60).is_err());
    }

    #[test]
    fn parse_status_with_grid_locator_and_symbol() -> TestResult {
        // Sub-format 3: spec example from §16 p.81.
        let info = b">IO91SX/G";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.grid_locator().map(MaidenheadGrid::as_str),
            Some("IO91SX")
        );
        assert_eq!(
            status.symbol(),
            Some(AprsSymbol::new(SymbolTable::Primary, b'G')?)
        );
        assert_eq!(status.text, "");
        Ok(())
    }

    #[test]
    fn parse_status_with_grid_locator_and_comment() -> TestResult {
        let info = b">IO91SX/G Net active";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.grid_locator().map(MaidenheadGrid::as_str),
            Some("IO91SX")
        );
        assert_eq!(
            status.symbol(),
            Some(AprsSymbol::new(SymbolTable::Primary, b'G')?)
        );
        assert_eq!(status.text, " Net active");
        Ok(())
    }

    #[test]
    fn parse_status_with_4char_grid() -> TestResult {
        let info = b">IO91/V";
        let status = parse_aprs_status(info)?;
        assert_eq!(
            status.grid_locator().map(MaidenheadGrid::as_str),
            Some("IO91")
        );
        assert_eq!(
            status.symbol(),
            Some(AprsSymbol::new(SymbolTable::Primary, b'V')?)
        );
        Ok(())
    }

    #[test]
    fn parse_status_with_invalid_grid_falls_through_to_text() -> TestResult {
        // "ZZZZZZ" is not a valid grid (field letters must be A..R).
        // The parser must fall through to plain-text mode rather than
        // emitting a spurious grid.
        let info = b">ZZZZZZ/G suspicious";
        let status = parse_aprs_status(info)?;
        assert_eq!(status.grid_locator, None);
        assert_eq!(status.symbol, None);
        assert_eq!(status.text, "ZZZZZZ/G suspicious");
        Ok(())
    }

    #[test]
    fn parse_status_timestamp_takes_precedence_over_grid() -> TestResult {
        // A 7-byte well-formed timestamp wins over a 6-byte grid match,
        // because the timestamp is the deterministic structured form
        // (suffix discriminates) and grids can superficially resemble
        // text prefixes.
        let info = b">092345zIO91SX is the grid";
        let status = parse_aprs_status(info)?;
        assert!(status.timestamp.is_some());
        // The grid-locator prefix is not extracted in this case; it
        // becomes part of `text` since the timestamp prefix consumed
        // the first 7 bytes.
        assert_eq!(status.text, "IO91SX is the grid");
        Ok(())
    }

    #[test]
    fn validate_grid_locator_accepts_valid_forms() {
        assert!(is_valid_grid_locator("AA00"));
        assert!(is_valid_grid_locator("IO91"));
        assert!(is_valid_grid_locator("RR99"));
        assert!(is_valid_grid_locator("IO91SX"));
        assert!(is_valid_grid_locator("AA00aa"));
        assert!(is_valid_grid_locator("RR99xx"));
    }

    #[test]
    fn validate_grid_locator_rejects_malformed() {
        assert!(!is_valid_grid_locator(""));
        assert!(!is_valid_grid_locator("ABC")); // wrong length
        assert!(!is_valid_grid_locator("ABCDE")); // wrong length
        assert!(!is_valid_grid_locator("ZZ00")); // field letter > R
        assert!(!is_valid_grid_locator("AAaa")); // square not digit
        assert!(!is_valid_grid_locator("AA00zz")); // sub-square letter > x (lowercase)
        assert!(!is_valid_grid_locator("AA00ZZ")); // sub-square letter > x (uppercase)
    }

    #[test]
    fn validate_grid_locator_accepts_mixed_case() {
        // Real-world traffic mixes cases; accept both for both halves
        // of the locator. Sub-square stays bounded at A..X (case-folded
        // to a..x); Y and Z are spec-disallowed in either case.
        assert!(is_valid_grid_locator("aa00"), "lowercase field");
        assert!(is_valid_grid_locator("AA00XX"), "uppercase sub-square at X");
        assert!(is_valid_grid_locator("Aa00bX"), "mixed case");
    }
}
