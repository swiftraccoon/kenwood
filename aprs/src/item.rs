//! APRS item reports, object reports, and queries (APRS 1.0.1 ch. 11 & 15).

use crate::error::AprsError;
use crate::position::{AprsPosition, parse_compressed_body, parse_uncompressed_body};

/// An APRS object report (data type `;`).
///
/// Objects represent entities that may not have their own radio —
/// hurricanes, marathon runners, event locations. They include a
/// name (9 chars), a live/killed flag, a timestamp, and a position.
///
/// Per User Manual Chapter 14: the TH-D75 can transmit Object
/// information via Menu No. 550 (Object 1-3).
#[derive(Debug, Clone, PartialEq)]
pub struct AprsObject {
    /// Object name (up to 9 characters).
    pub name: String,
    /// Whether the object is live (`true`) or killed (`false`).
    pub live: bool,
    /// DHM or HMS timestamp from the object report (7 characters).
    pub timestamp: String,
    /// Position data.
    pub position: AprsPosition,
}

/// An APRS item report (data type `)` ).
///
/// Items are similar to objects but simpler — no timestamp. They
/// represent static entities like event locations or landmarks.
#[derive(Debug, Clone, PartialEq)]
pub struct AprsItem {
    /// Item name (3-9 characters).
    pub name: String,
    /// Whether the item is live (`true`) or killed (`false`).
    pub live: bool,
    /// Position data.
    pub position: AprsPosition,
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
    /// Weather query (`?WX`) — request latest weather fields.
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
    Other(String),
}

/// Parse an APRS object report (`;name_____*DDHHMMzpos...`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field is shorter
/// than 27 bytes, is missing the leading `;`, has an invalid live/killed
/// flag, or has malformed position data.
pub fn parse_aprs_object(info: &[u8]) -> Result<AprsObject, AprsError> {
    if info.first() != Some(&b';') {
        return Err(AprsError::InvalidFormat);
    }
    // ; + 9-char name + * or _ + 7-char timestamp + position (≥8 bytes) = 27 min
    let name_bytes = info.get(1..10).ok_or(AprsError::InvalidFormat)?;
    let flag = *info.get(10).ok_or(AprsError::InvalidFormat)?;
    let live = match flag {
        b'*' => true,
        b'_' => false,
        _ => return Err(AprsError::InvalidFormat),
    };

    let name = String::from_utf8_lossy(name_bytes).trim().to_string();

    // After the name and live/killed flag, there's a 7-char timestamp
    // then position data.
    let pos_body = info.get(11..).ok_or(AprsError::InvalidFormat)?;
    let ts_bytes = pos_body.get(..7).ok_or(AprsError::InvalidFormat)?;
    let timestamp = String::from_utf8_lossy(ts_bytes).to_string();
    let pos_data = pos_body.get(7..).ok_or(AprsError::InvalidFormat)?;

    let first = *pos_data.first().ok_or(AprsError::InvalidFormat)?;
    let position = if first.is_ascii_digit() {
        parse_uncompressed_body(pos_data)?
    } else {
        parse_compressed_body(pos_data)?
    };

    Ok(AprsObject {
        name,
        live,
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
pub fn parse_aprs_item(info: &[u8]) -> Result<AprsItem, AprsError> {
    if info.first() != Some(&b')') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).ok_or(AprsError::InvalidFormat)?;

    // Scan the first 9 bytes for a terminator. Anything beyond that is
    // outside the spec-legal range and the frame is malformed.
    let search_len = std::cmp::min(body.len(), 9);
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
    let live = term_byte == b'!';
    let name_bytes = body.get(..terminator_pos).ok_or(AprsError::InvalidFormat)?;
    // Reject non-printable ASCII in names.
    if name_bytes.iter().any(|&b| !(0x20..=0x7E).contains(&b)) {
        return Err(AprsError::InvalidFormat);
    }
    let name = std::str::from_utf8(name_bytes)
        .map_err(|_| AprsError::InvalidFormat)?
        .to_owned();
    let pos_data = body
        .get(terminator_pos + 1..)
        .ok_or(AprsError::InvalidFormat)?;

    let first = *pos_data.first().ok_or(AprsError::InvalidFormat)?;
    let position = if first.is_ascii_digit() {
        parse_uncompressed_body(pos_data)?
    } else {
        parse_compressed_body(pos_data)?
    };

    Ok(AprsItem {
        name,
        live,
        position,
    })
}

/// Parse an APRS query (`?APRSx` or `?text`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not begin
/// with `?`.
pub fn parse_aprs_query(info: &[u8]) -> Result<AprsQuery, AprsError> {
    if info.first() != Some(&b'?') {
        return Err(AprsError::InvalidFormat);
    }

    let body_bytes = info.get(1..).unwrap_or(&[]);
    let body = String::from_utf8_lossy(body_bytes);
    let text = body.trim_end_matches('\r');

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
        _ => Ok(AprsQuery::Other(text.to_owned())),
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
        assert_eq!(obj.name, "TORNADO");
        assert!(obj.live);
        assert_eq!(obj.timestamp, "092345z");
        assert!(
            (obj.position.latitude - 49.058_333).abs() < 0.001,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_object_killed() -> TestResult {
        let info = b";MARATHON _092345z4903.50N/07201.75W-Event over";
        let obj = parse_aprs_object(info)?;
        assert_eq!(obj.name, "MARATHON");
        assert!(!obj.live);
        Ok(())
    }

    #[test]
    fn parse_object_rejects_bad_live_flag() {
        let info = b";TORNADO  X092345z4903.50N/07201.75W-";
        assert!(parse_aprs_object(info).is_err(), "bad flag must error");
    }

    // ---- APRS item tests ----

    #[test]
    fn parse_item_live() -> TestResult {
        let info = b")AID#2!4903.50N/07201.75W-First aid";
        let item = parse_aprs_item(info)?;
        assert_eq!(item.name, "AID#2");
        assert!(item.live);
        assert!(
            (item.position.latitude - 49.058_333).abs() < 0.001,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_item_killed() -> TestResult {
        let info = b")AID#2_4903.50N/07201.75W-Closed";
        let item = parse_aprs_item(info)?;
        assert!(!item.live);
        Ok(())
    }

    #[test]
    fn parse_item_short_name_rejected() {
        // "AB" is only 2 characters — APRS101 requires 3-9.
        let info = b")AB!4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_item(info), Err(AprsError::InvalidFormat)),
            "short name must be rejected",
        );
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
            AprsQuery::Other("FOOBAR".to_owned())
        );
        Ok(())
    }

    #[test]
    fn parse_query_with_trailing_cr() -> TestResult {
        assert_eq!(parse_aprs_query(b"?APRSP\r")?, AprsQuery::Position);
        Ok(())
    }
}
