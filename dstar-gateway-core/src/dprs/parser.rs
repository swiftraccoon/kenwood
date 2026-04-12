//! DPRS sentence parser.

use crate::types::Callsign;

use super::coordinates::{Latitude, Longitude};
use super::error::DprsError;

/// A parsed DPRS position report.
#[derive(Debug, Clone, PartialEq)]
pub struct DprsReport {
    /// Station callsign.
    pub callsign: Callsign,
    /// Latitude in decimal degrees.
    pub latitude: Latitude,
    /// Longitude in decimal degrees.
    pub longitude: Longitude,
    /// Symbol character (APRS symbol code).
    pub symbol: char,
    /// Optional comment text.
    pub comment: Option<String>,
}

/// Parse a DPRS sentence into a [`DprsReport`].
///
/// The sentence must start with `$$CRC<4hex>,`. The CRC field is
/// parsed but not validated here — callers that want to verify it
/// should compute [`super::compute_crc`] over the body bytes after
/// the comma and compare against the 4-hex value between `$$CRC`
/// and `,`.
///
/// # Errors
///
/// - [`DprsError::MissingCrcPrefix`] if the sentence doesn't start with `$$CRC`
/// - [`DprsError::TooShort`] if shorter than the minimum viable length
/// - [`DprsError::MalformedCoordinates`] if lat/lon fields fail to parse
/// - [`DprsError::LatitudeOutOfRange`] / [`DprsError::LongitudeOutOfRange`]
/// - [`DprsError::InvalidCallsign`] if the callsign field is invalid
///
/// # See also
///
/// `ircDDBGateway/Common/APRSCollector.cpp:371-394` — the reference
/// parser this decoder mirrors. CRC-CCITT uses reflected polynomial
/// `0x8408`, initial value `0xFFFF`, final `~accumulator`.
pub fn parse_dprs(sentence: &str) -> Result<DprsReport, DprsError> {
    if !sentence.starts_with("$$CRC") {
        return Err(DprsError::MissingCrcPrefix);
    }
    if sentence.len() < 40 {
        return Err(DprsError::TooShort {
            got: sentence.len(),
        });
    }

    // Format:
    //   "$$CRCXXXX,W1AW    *>APDPRS,DSTAR*:!DDMM.MMN/DDDMM.MMW#/comment"
    //    0         10       18
    //
    // Skip past "$$CRC" + 4 hex digits + ",".
    let after_crc = sentence.get(10..).ok_or(DprsError::MalformedCoordinates)?;

    // Callsign is the first 8 bytes of `after_crc` (space-padded).
    let cs_bytes = after_crc.get(..8).ok_or(DprsError::MalformedCoordinates)?;
    let callsign =
        Callsign::try_from_str(cs_bytes.trim_end()).map_err(|_| DprsError::InvalidCallsign {
            reason: "not a valid D-STAR callsign",
        })?;

    // Skip to the '!' character that marks the start of position data.
    let bang_pos = sentence.find('!').ok_or(DprsError::MalformedCoordinates)?;
    let pos_data = sentence
        .get(bang_pos + 1..)
        .ok_or(DprsError::MalformedCoordinates)?;

    // `pos_data` format: "DDMM.MMN/DDDMM.MMW#/comment"
    //  index 0..8  = latitude (DDMM.MM + N/S)
    //  index 8     = '/' separator
    //  index 9..18 = longitude (DDDMM.MM + E/W)
    //  index 18    = symbol-table overlay (e.g. '#')
    //  index 19    = symbol glyph (e.g. '/')
    //  index 20..  = optional comment
    if pos_data.len() < 19 {
        return Err(DprsError::MalformedCoordinates);
    }

    let lat_str = pos_data.get(..8).ok_or(DprsError::MalformedCoordinates)?;
    let lat_hemi = lat_str
        .chars()
        .nth(7)
        .ok_or(DprsError::MalformedCoordinates)?;
    let lat_numeric = lat_str.get(..7).ok_or(DprsError::MalformedCoordinates)?;
    let lat_deg = parse_aprs_degrees(lat_numeric, 2).ok_or(DprsError::MalformedCoordinates)?;
    let lat_deg = if lat_hemi == 'S' { -lat_deg } else { lat_deg };
    let latitude = Latitude::try_new(lat_deg)?;

    let lon_str = pos_data.get(9..18).ok_or(DprsError::MalformedCoordinates)?;
    let lon_hemi = lon_str
        .chars()
        .nth(8)
        .ok_or(DprsError::MalformedCoordinates)?;
    let lon_numeric = lon_str.get(..8).ok_or(DprsError::MalformedCoordinates)?;
    let lon_deg = parse_aprs_degrees(lon_numeric, 3).ok_or(DprsError::MalformedCoordinates)?;
    let lon_deg = if lon_hemi == 'W' { -lon_deg } else { lon_deg };
    let longitude = Longitude::try_new(lon_deg)?;

    // Symbol glyph is at [19]; absent when the sentence ends at the
    // overlay (e.g. the `parse_without_comment` test case).
    let symbol = pos_data.chars().nth(19).unwrap_or('/');
    let comment_str = pos_data.get(20..).map(ToString::to_string);
    let comment = comment_str.filter(|s| !s.is_empty());

    Ok(DprsReport {
        callsign,
        latitude,
        longitude,
        symbol,
        comment,
    })
}

/// Parse an APRS "DDMM.MM" or "DDDMM.MM" numeric string into decimal degrees.
///
/// `degree_digits` is 2 for latitude (DDMM.MM) or 3 for longitude (DDDMM.MM).
fn parse_aprs_degrees(s: &str, degree_digits: usize) -> Option<f64> {
    if s.len() < degree_digits + 5 {
        return None;
    }
    let deg_str = s.get(..degree_digits)?;
    let min_str = s.get(degree_digits..)?;
    let degrees = deg_str.parse::<f64>().ok()?;
    let minutes = min_str.parse::<f64>().ok()?;
    Some(degrees + minutes / 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_w1aw_asheville() -> TestResult {
        // Synthesized sentence for W1AW at 35.5N 82.55W
        let sentence = "$$CRC0000,W1AW    *>APDPRS,DSTAR*:!3530.00N/08233.00W#/Asheville test";
        let report = parse_dprs(sentence)?;
        assert_eq!(report.callsign.as_str().trim(), "W1AW");
        assert!((report.latitude.degrees() - 35.5).abs() < 0.001);
        assert!((report.longitude.degrees() - (-82.55)).abs() < 0.001);
        assert_eq!(report.symbol, '/');
        assert_eq!(report.comment.as_deref(), Some("Asheville test"));
        Ok(())
    }

    #[test]
    fn parse_missing_crc_prefix_errors() {
        let sentence = "HELLO,W1AW    *>APDPRS,DSTAR*:!3530.00N/08233.00W#/";
        let result = parse_dprs(sentence);
        assert!(
            matches!(result, Err(DprsError::MissingCrcPrefix)),
            "expected MissingCrcPrefix, got {result:?}"
        );
    }

    #[test]
    fn parse_too_short_errors() {
        let sentence = "$$CRC1234,short";
        let result = parse_dprs(sentence);
        assert!(
            matches!(result, Err(DprsError::TooShort { .. })),
            "expected TooShort, got {result:?}"
        );
    }

    #[test]
    fn parse_southern_hemisphere() -> TestResult {
        let sentence = "$$CRC0000,VK2ABC  *>APDPRS,DSTAR*:!3351.00S/15112.00E#/Sydney";
        let report = parse_dprs(sentence)?;
        assert!(
            report.latitude.degrees() < 0.0,
            "southern hemisphere is negative"
        );
        assert!(
            report.longitude.degrees() > 0.0,
            "eastern hemisphere is positive"
        );
        Ok(())
    }

    #[test]
    fn parse_without_comment() -> TestResult {
        let sentence = "$$CRC0000,W1AW    *>APDPRS,DSTAR*:!3530.00N/08233.00W#";
        let report = parse_dprs(sentence)?;
        assert!(report.comment.is_none());
        Ok(())
    }

    #[test]
    fn parse_extreme_latitude() -> TestResult {
        // 90.00N
        let sentence = "$$CRC0000,NORTH   *>APDPRS,DSTAR*:!9000.00N/00000.00E#/North Pole";
        let report = parse_dprs(sentence)?;
        assert!((report.latitude.degrees() - 90.0).abs() < 0.001);
        Ok(())
    }
}
