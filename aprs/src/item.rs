//! APRS item reports, object reports, and queries (APRS 1.0.1 ch. 11 & 15).

use crate::error::AprsError;
use crate::packet::AprsReportTimestamp;
use crate::position::AprsPosition;
use crate::text::{ItemName, ObjectName, QueryText, decode_wire_ascii};

/// An APRS object report (data type `;`).
///
/// Objects represent entities that may not have their own radio:
/// hurricanes, marathon runners, event locations. They include a
/// name (9 chars), a live/killed flag, a timestamp, and a position.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsObject {
    /// Validated object name (up to 9 printable ASCII bytes).
    name: ObjectName,
    /// Live or killed state advertised by the report.
    state: ReportState,
    /// Required seven-byte DHM or HMS timestamp.
    timestamp: AprsReportTimestamp,
    /// Position data.
    position: AprsPosition,
}

/// Whether an object or item is advertised as live or withdrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportState {
    /// The reported object or item is live.
    Live,
    /// The report withdraws the object or item.
    Killed,
}

impl ReportState {
    /// Returns whether the report advertises a live object or item.
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

impl AprsObject {
    /// Retains a checked name, state, report timestamp and position observation.
    #[must_use]
    pub const fn new(
        name: ObjectName,
        state: ReportState,
        timestamp: AprsReportTimestamp,
        position: AprsPosition,
    ) -> Self {
        Self {
            name,
            state,
            timestamp,
            position,
        }
    }
    /// Borrows the unpadded object name.
    #[must_use]
    pub const fn name(&self) -> &ObjectName {
        &self.name
    }
    /// Returns the live or killed state.
    #[must_use]
    pub const fn state(&self) -> ReportState {
        self.state
    }
    /// Returns the object report's timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> AprsReportTimestamp {
        self.timestamp
    }
    /// Borrows the position observation.
    #[must_use]
    pub const fn position(&self) -> &AprsPosition {
        &self.position
    }
    /// Decodes a padded name, state, seven-byte timestamp and position.
    ///
    /// # Errors
    /// Returns framing, name, timestamp or position errors from [`AprsError`].
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_object(info)
    }
}

/// An APRS item report (data type `)` ).
///
/// Items are similar to objects but simpler (no timestamp). They
/// represent static entities like event locations or landmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsItem {
    /// Item name (3-9 characters).
    name: ItemName,
    /// Live or killed state advertised by the report.
    state: ReportState,
    /// Position data.
    position: AprsPosition,
}

impl AprsItem {
    /// Retains a checked name, state and position observation.
    #[must_use]
    pub const fn new(name: ItemName, state: ReportState, position: AprsPosition) -> Self {
        Self {
            name,
            state,
            position,
        }
    }
    /// Borrows the item name.
    #[must_use]
    pub const fn name(&self) -> &ItemName {
        &self.name
    }
    /// Returns the live or killed state.
    #[must_use]
    pub const fn state(&self) -> ReportState {
        self.state
    }
    /// Borrows the position observation.
    #[must_use]
    pub const fn position(&self) -> &AprsPosition {
        &self.position
    }
    /// Decodes a three-to-nine-byte name, live/killed delimiter and position.
    ///
    /// # Errors
    /// Returns [`AprsError::InvalidFormat`] for invalid framing, names or
    /// delimiters, or the position decoder's error.
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_item(info)
    }
}

/// Parsed APRS query.
///
/// Per APRS 1.0.1 Chapter 15, queries start with `?` and allow stations
/// to request information from other stations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AprsQuery {
    /// Position query (`?APRSP` or `?APRS?`).
    Position,
    /// Status query (`?APRSS`).
    Status,
    /// Message query for a specific callsign (`?APRSM`).
    Message,
    /// Direction finding query (`?APRSD`).
    DirectionFinding,
    /// Weather query (`?WX`): request latest weather fields.
    Weather,
    /// Telemetry query (`?APRST` or `?APRST?`).
    Telemetry,
    /// Ping query (`?PING?` or `?PING`).
    Ping,
    /// `IGate` query (`?IGATE?` or `?IGATE`).
    IGate,
    /// Stations-heard-on-RF query (`?APRSH`).
    Heard,
    /// General query with raw text (everything after the leading `?`,
    /// not one of the well-known forms).
    Other(QueryText),
}

impl AprsQuery {
    /// Decodes a query's leading `?` and recognized selector or retained body.
    ///
    /// # Errors
    /// Returns [`AprsError::InvalidFormat`] without `?`, or
    /// [`AprsError::InvalidTextByte`] for non-ASCII bytes.
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_query(info)
    }
}

/// Parse an APRS object report (`;name_____*DDHHMMzpos...`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field is shorter
/// than 27 bytes, is missing the leading `;`, has an invalid live/killed
/// flag, or has malformed position data.
pub(crate) fn parse_aprs_object(info: &[u8]) -> Result<AprsObject, AprsError> {
    if info.first() != Some(&b';') {
        return Err(AprsError::InvalidFormat);
    }
    // ; + 9-char name + * or _ + 7-char timestamp + position (≥8 bytes) = 27 min
    let name_bytes = info.get(1..10).ok_or(AprsError::InvalidFormat)?;
    let flag = *info.get(10).ok_or(AprsError::InvalidFormat)?;
    let state = match flag {
        b'*' => ReportState::Live,
        b'_' => ReportState::Killed,
        _ => return Err(AprsError::InvalidFormat),
    };

    let name_wire = decode_wire_ascii(crate::AprsWireTextField::ObjectName, name_bytes)?;
    let name =
        ObjectName::new(name_wire.trim_end_matches(' ')).map_err(AprsError::InvalidObjectName)?;

    // After the name and live/killed flag, there's a 7-char timestamp
    // then position data.
    let pos_body = info.get(11..).ok_or(AprsError::InvalidFormat)?;
    let ts_bytes = pos_body.get(..7).ok_or(AprsError::InvalidFormat)?;
    let timestamp_wire = decode_wire_ascii(crate::AprsWireTextField::ObjectTimestamp, ts_bytes)?;
    let timestamp = AprsReportTimestamp::from_wire(timestamp_wire)?;
    let pos_data = pos_body.get(7..).ok_or(AprsError::InvalidFormat)?;

    let first = *pos_data.first().ok_or(AprsError::InvalidFormat)?;
    let position = if first.is_ascii_digit() {
        AprsPosition::from_uncompressed_body(pos_data)?
    } else {
        AprsPosition::from_compressed_body(pos_data)?
    };

    Ok(AprsObject {
        name,
        state,
        timestamp,
        position,
    })
}

/// Parse an APRS item report (`)name!pos...` or `)name_pos...`).
///
/// Per APRS 1.0.1 Chapter 11, the name is 3-9 characters terminated by
/// `!` (live) or `_` (killed). The name is restricted to printable
/// ASCII excluding the terminator characters themselves.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for any violation of the spec:
/// missing leading `)`, name outside the 3-9 character range, missing
/// terminator, non-printable ASCII in name, or malformed position data.
pub(crate) fn parse_aprs_item(info: &[u8]) -> Result<AprsItem, AprsError> {
    if info.first() != Some(&b')') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).ok_or(AprsError::InvalidFormat)?;

    // The delimiter follows a name of at most nine bytes.
    let search_len = std::cmp::min(body.len(), ItemName::MAX_LEN + 1);
    let search = body.get(..search_len).ok_or(AprsError::InvalidFormat)?;
    let terminator_pos = search
        .iter()
        .position(|&b| b == b'!' || b == b'_')
        .ok_or(AprsError::InvalidFormat)?;

    // Names are 3-9 characters inclusive.
    if terminator_pos < 3 {
        return Err(AprsError::InvalidFormat);
    }

    let term_byte = *body.get(terminator_pos).ok_or(AprsError::InvalidFormat)?;
    let state = if term_byte == b'!' {
        ReportState::Live
    } else {
        ReportState::Killed
    };
    let name_bytes = body.get(..terminator_pos).ok_or(AprsError::InvalidFormat)?;
    // Reject non-printable ASCII in names.
    if name_bytes.iter().any(|&b| !(0x20..=0x7E).contains(&b)) {
        return Err(AprsError::InvalidFormat);
    }
    let name =
        ItemName::new(std::str::from_utf8(name_bytes).map_err(|_| AprsError::InvalidFormat)?)
            .map_err(|_| AprsError::InvalidFormat)?;
    let pos_data = body
        .get(terminator_pos + 1..)
        .ok_or(AprsError::InvalidFormat)?;

    let first = *pos_data.first().ok_or(AprsError::InvalidFormat)?;
    let position = if first.is_ascii_digit() {
        AprsPosition::from_uncompressed_body(pos_data)?
    } else {
        AprsPosition::from_compressed_body(pos_data)?
    };

    Ok(AprsItem {
        name,
        state,
        position,
    })
}

/// Parse an APRS query (`?APRSx` or `?text`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not begin
/// with `?`.
pub(crate) fn parse_aprs_query(info: &[u8]) -> Result<AprsQuery, AprsError> {
    if info.first() != Some(&b'?') {
        return Err(AprsError::InvalidFormat);
    }

    let body_bytes = info.get(1..).unwrap_or(&[]);
    let body = decode_wire_ascii(crate::AprsWireTextField::QueryBody, body_bytes)?;
    let text = body.trim_end_matches(['\r', '\n']);

    // Standard APRS queries per APRS 1.0.1 Chapter 15.
    match text {
        "APRSP" | "APRS?" => Ok(AprsQuery::Position),
        "APRSS" => Ok(AprsQuery::Status),
        "APRSM" => Ok(AprsQuery::Message),
        "APRSD" => Ok(AprsQuery::DirectionFinding),
        "APRST" | "APRST?" => Ok(AprsQuery::Telemetry),
        "APRSH" => Ok(AprsQuery::Heard),
        "WX" => Ok(AprsQuery::Weather),
        "PING" | "PING?" => Ok(AprsQuery::Ping),
        "IGATE" | "IGATE?" => Ok(AprsQuery::IGate),
        _ => Ok(AprsQuery::Other(
            QueryText::new(text).map_err(|_| AprsError::InvalidFormat)?,
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- APRS object tests ----

    #[test]
    fn parse_object_live() -> TestResult {
        let info = b";TORNADO  *092345z4903.50N/07201.75W-Tornado warning";
        let obj = parse_aprs_object(info)?;
        assert_eq!(obj.name.as_str(), "TORNADO");
        assert!(obj.state().is_live());
        assert_eq!(obj.timestamp.to_wire_string(), "092345z");
        assert!(
            (obj.position().latitude().as_degrees() - 49.058_333).abs() < 0.001,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_object_killed() -> TestResult {
        let info = b";MARATHON _092345z4903.50N/07201.75W-Event over";
        let obj = parse_aprs_object(info)?;
        assert_eq!(obj.name.as_str(), "MARATHON");
        assert!(!obj.state().is_live());
        Ok(())
    }

    #[test]
    fn parse_object_rejects_bad_live_flag() {
        let info = b";TORNADO  X092345z4903.50N/07201.75W-";
        assert!(parse_aprs_object(info).is_err(), "bad flag must error");
    }

    #[test]
    fn parse_object_rejects_malformed_report_timestamp() {
        let info = b";TORNADO  *092345X4903.50N/07201.75W-";

        assert_eq!(
            parse_aprs_object(info),
            Err(AprsError::InvalidTimestamp(
                crate::TimestampError::UnknownSuffix,
            )),
        );
    }

    #[test]
    fn parse_object_rejects_eight_byte_weather_timestamp() {
        let info = b";TORNADO  *010112344903.50N/07201.75W-";

        assert!(
            matches!(parse_aprs_object(info), Err(AprsError::InvalidTimestamp(_))),
            "object accepted the positionless-weather MMDDHHMM timestamp",
        );
    }

    #[test]
    fn parse_object_rejects_non_ascii_name_without_replacement() -> TestResult {
        let mut info = b";TORNADO  *092345z4903.50N/07201.75W-".to_vec();
        *info
            .get_mut(1)
            .ok_or("static object fixture must contain its first name byte")? = 0xFF;

        assert_eq!(
            parse_aprs_object(&info),
            Err(AprsError::InvalidTextByte {
                field: crate::AprsWireTextField::ObjectName,
                index: 0,
                byte: 0xFF,
            }),
        );
        Ok(())
    }

    // ---- APRS item tests ----

    #[test]
    fn parse_item_live() -> TestResult {
        let info = b")AID#2!4903.50N/07201.75W-First aid";
        let item = parse_aprs_item(info)?;
        assert_eq!(item.name.as_str(), "AID#2");
        assert!(item.state().is_live());
        assert!(
            (item.position().latitude().as_degrees() - 49.058_333).abs() < 0.001,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_item_killed() -> TestResult {
        let info = b")AID#2_4903.50N/07201.75W-Closed";
        let item = parse_aprs_item(info)?;
        assert!(!item.state().is_live());
        Ok(())
    }

    #[test]
    fn parse_item_short_name_rejected() {
        // "AB" is only 2 characters; APRS101 requires 3-9.
        let info = b")AB!4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_item(info), Err(AprsError::InvalidFormat)),
            "short name must be rejected",
        );
    }

    #[test]
    fn item_name_boundaries_include_the_terminal_delimiter() -> TestResult {
        for length in [3, 8, 9, 10] {
            for live in [true, false] {
                let name = "A".repeat(length);
                let delimiter = if live { '!' } else { '_' };
                let wire = format!("){name}{delimiter}4903.50N/07201.75W-");
                let parsed = parse_aprs_item(wire.as_bytes());
                if length <= ItemName::MAX_LEN {
                    let item = parsed?;
                    assert_eq!(item.name.as_str(), name);
                    assert_eq!(item.state().is_live(), live);
                } else {
                    assert!(parsed.is_err(), "ten-byte item name accepted");
                }
            }
        }
        Ok(())
    }

    // ---- APRS query tests ----

    #[test]
    fn parse_query_position_aprsp() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSP")?, AprsQuery::Position);
        Ok(())
    }

    #[test]
    fn parse_query_position_aprs_question() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRS?")?, AprsQuery::Position);
        Ok(())
    }

    #[test]
    fn parse_query_status() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSS")?, AprsQuery::Status);
        Ok(())
    }

    #[test]
    fn parse_query_message() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSM")?, AprsQuery::Message);
        Ok(())
    }

    #[test]
    fn parse_query_direction_finding() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSD")?, AprsQuery::DirectionFinding);
        Ok(())
    }

    #[test]
    fn parse_query_igate() -> TestResult {
        assert_eq!(parse_aprs_query(b"?IGATE")?, AprsQuery::IGate);
        Ok(())
    }

    #[test]
    fn parse_query_ping() -> TestResult {
        assert_eq!(parse_aprs_query(b"?PING?")?, AprsQuery::Ping);
        Ok(())
    }

    #[test]
    fn parse_query_weather() -> TestResult {
        assert_eq!(parse_aprs_query(b"?WX")?, AprsQuery::Weather);
        Ok(())
    }

    #[test]
    fn parse_query_telemetry() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRST")?, AprsQuery::Telemetry);
        Ok(())
    }

    #[test]
    fn parse_query_heard() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSH")?, AprsQuery::Heard);
        Ok(())
    }

    #[test]
    fn parse_query_other() -> TestResult {
        assert_eq!(
            parse_aprs_query(b"?FOOBAR")?,
            AprsQuery::Other(QueryText::new("FOOBAR")?)
        );
        Ok(())
    }

    #[test]
    fn parse_query_with_trailing_cr() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSP\r")?, AprsQuery::Position);
        Ok(())
    }

    #[test]
    fn parse_query_rejects_non_ascii_body_without_replacement() {
        assert_eq!(
            parse_aprs_query(b"?PING\xFF"),
            Err(AprsError::InvalidTextByte {
                field: crate::AprsWireTextField::QueryBody,
                index: 4,
                byte: 0xFF,
            }),
        );
    }
}
