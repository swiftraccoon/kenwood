//! DPRS sentence encoder.

use std::fmt::Write as _;

use super::crc::compute_crc;
use super::error::DprsError;
use super::parser::DprsReport;

/// Encode a `DprsReport` into a DPRS sentence with a correct
/// `$$CRC<hex>` checksum.
///
/// The output `String` is cleared first, then written in place. The
/// CRC is computed over the sentence body (everything after the
/// comma following `$$CRC<hex>`) using [`super::compute_crc`] —
/// CRC-CCITT with reflected polynomial `0x8408`, initial value
/// `0xFFFF`, final `~accumulator`, matching the ircDDBGateway
/// reference.
///
/// # Errors
///
/// Returns [`DprsError::MalformedCoordinates`] if the report's lat/lon
/// values can't be formatted. This should not happen with validated
/// [`super::coordinates::Latitude`] / [`super::coordinates::Longitude`]
/// newtypes.
///
/// # See also
///
/// `ircDDBGateway/Common/APRSCollector.cpp:371-394` for the
/// reference CRC + sentence layout this encoder mirrors.
pub fn encode_dprs(report: &DprsReport, out: &mut String) -> Result<(), DprsError> {
    // Build the sentence body (everything that comes after the CRC
    // prefix + comma) in a scratch buffer first so we can compute
    // the CRC over it, then emit the whole sentence with the real
    // CRC prefixed.
    let mut body = String::new();

    // Callsign (space-padded to 8 bytes) — read straight from the
    // wire bytes so we don't depend on `Callsign::as_str()`'s
    // trimming behaviour.
    let cs_bytes = report.callsign.as_bytes();
    for &b in cs_bytes {
        body.push(char::from(b));
    }

    body.push_str("*>APDPRS,DSTAR*:!");

    // Latitude DDMM.MM[NS]
    let lat = report.latitude.degrees();
    let lat_hemi = if lat < 0.0 { 'S' } else { 'N' };
    let lat_abs = lat.abs();
    let lat_int = lat_abs.trunc();
    let lat_min = (lat_abs - lat_int) * 60.0;
    // Width 2, leading zeros, precision 0 — prints e.g. "35" for
    // 35.0. `lat_int` has already been truncated, so `{:.0}` does
    // not round away from the integer-degree value.
    write!(body, "{lat_int:02.0}{lat_min:05.2}{lat_hemi}")
        .map_err(|_| DprsError::MalformedCoordinates)?;

    body.push('/');

    // Longitude DDDMM.MM[EW]
    let lon = report.longitude.degrees();
    let lon_hemi = if lon < 0.0 { 'W' } else { 'E' };
    let lon_abs = lon.abs();
    let lon_int = lon_abs.trunc();
    let lon_min = (lon_abs - lon_int) * 60.0;
    write!(body, "{lon_int:03.0}{lon_min:05.2}{lon_hemi}")
        .map_err(|_| DprsError::MalformedCoordinates)?;

    // Overlay '#' + symbol glyph.
    body.push('#');
    body.push(report.symbol);

    if let Some(comment) = &report.comment {
        body.push_str(comment);
    }

    // Now compute the CRC over the body and emit the final sentence.
    let crc = compute_crc(body.as_bytes());
    out.clear();
    write!(out, "$$CRC{crc:04X},").map_err(|_| DprsError::MalformedCoordinates)?;
    out.push_str(&body);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dprs::coordinates::{Latitude, Longitude};
    use crate::types::Callsign;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn encode_round_trips_through_parser() -> TestResult {
        let original = DprsReport {
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            latitude: Latitude::try_new(35.5)?,
            longitude: Longitude::try_new(-82.55)?,
            symbol: '/',
            comment: Some("Asheville test".to_string()),
        };
        let mut encoded = String::new();
        encode_dprs(&original, &mut encoded)?;
        // Sentence must start with `$$CRC<4hex>,` (the hex is the
        // real CRC-CCITT over the body, not a placeholder).
        assert!(encoded.starts_with("$$CRC"));
        assert_eq!(
            encoded.as_bytes().get(9),
            Some(&b','),
            "comma after 4 hex digits"
        );
        // And the 4 CRC chars must be uppercase ASCII hex digits.
        for b in encoded.as_bytes().get(5..9).unwrap_or(&[]) {
            assert!(b.is_ascii_hexdigit(), "CRC byte {b:02X} is not hex");
        }
        assert!(encoded.contains("W1AW"));
        assert!(encoded.contains("3530.00N"));
        assert!(encoded.contains("08233.00W"));
        assert!(encoded.contains("Asheville test"));
        Ok(())
    }

    #[test]
    fn encode_crc_matches_compute_crc_over_body() -> TestResult {
        use super::super::crc::compute_crc;
        let original = DprsReport {
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            latitude: Latitude::try_new(35.5)?,
            longitude: Longitude::try_new(-82.55)?,
            symbol: '/',
            comment: Some("Asheville test".to_string()),
        };
        let mut encoded = String::new();
        encode_dprs(&original, &mut encoded)?;
        // The body is everything after `$$CRC<4hex>,`.
        let body = encoded.get(10..).ok_or("body after prefix")?;
        let expected_crc = compute_crc(body.as_bytes());
        let crc_str = encoded.get(5..9).ok_or("crc field")?;
        let actual_crc = u16::from_str_radix(crc_str, 16)?;
        assert_eq!(actual_crc, expected_crc);
        Ok(())
    }

    #[test]
    fn encode_then_parse_roundtrip() -> TestResult {
        let original = DprsReport {
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            latitude: Latitude::try_new(35.5)?,
            longitude: Longitude::try_new(-82.55)?,
            symbol: '/',
            comment: None,
        };
        let mut encoded = String::new();
        encode_dprs(&original, &mut encoded)?;
        let parsed = super::super::parser::parse_dprs(&encoded)?;
        assert_eq!(parsed.callsign, original.callsign);
        assert!((parsed.latitude.degrees() - original.latitude.degrees()).abs() < 0.001);
        assert!((parsed.longitude.degrees() - original.longitude.degrees()).abs() < 0.001);
        Ok(())
    }
}
