//! APRS position reports (uncompressed and compressed).

use crate::error::AprsError;
use crate::mic_e::MiceMessage;
use crate::packet::{AprsDataExtension, PositionAmbiguity, parse_aprs_extensions};
use crate::weather::{AprsWeather, extract_position_weather};

/// A parsed APRS position report.
///
/// Includes optional speed/course fields populated by Mic-E decoding and
/// optional embedded weather data populated when the station reports with
/// the weather-station symbol code `_`. Data extensions (course/speed,
/// PHG, altitude, DAO) found in the comment field are parsed
/// automatically and exposed via [`Self::extensions`].
#[derive(Debug, Clone, PartialEq)]
pub struct AprsPosition {
    /// Latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = East).
    pub longitude: f64,
    /// APRS symbol table identifier character.
    pub symbol_table: char,
    /// APRS symbol code character.
    pub symbol_code: char,
    /// Speed in knots (from Mic-E or course/speed extension).
    pub speed_knots: Option<u16>,
    /// Course in degrees (from Mic-E or course/speed extension).
    pub course_degrees: Option<u16>,
    /// Optional comment/extension text after the position.
    pub comment: String,
    /// Optional weather data embedded in the position comment.
    ///
    /// Populated when the symbol code is `_` (weather station) and the
    /// comment starts with the `DDD/SSS` wind direction/speed extension,
    /// followed by the remaining weather fields. See APRS 1.0.1 §12.1.
    pub weather: Option<AprsWeather>,
    /// Parsed data extensions (course/speed, PHG, altitude, DAO) found in
    /// the comment field.
    ///
    /// Populated automatically by [`parse_aprs_position`] via
    /// [`parse_aprs_extensions`]. Fields that aren't present in the
    /// comment are `None`.
    pub extensions: AprsDataExtension,
    /// Mic-E standard message code (only populated by
    /// [`crate::mic_e::parse_mice_position`]).
    pub mice_message: Option<MiceMessage>,
    /// Mic-E altitude in metres, decoded from the comment per APRS 1.0.1
    /// §10.1.1 (three base-91 chars followed by `}`, offset from -10000).
    pub mice_altitude_m: Option<i32>,
    /// Position ambiguity level (APRS 1.0.1 §8.1.6).
    ///
    /// Stations can deliberately reduce their precision by replacing
    /// trailing lat/lon digits with spaces; this field records how many
    /// digits were masked. Mic-E and compressed positions do not use
    /// ambiguity and always report [`PositionAmbiguity::None`].
    pub ambiguity: PositionAmbiguity,
}

/// Parse APRS latitude from the standard `DDMM.HH[N/S]` format.
///
/// Returns `(degrees, ambiguity)` where `degrees` is the decimal-degree
/// value (positive North) and `ambiguity` counts how many trailing
/// digits were replaced with spaces per APRS 1.0.1 §8.1.6.
fn parse_aprs_latitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    let bytes_slice = s.get(..8).ok_or(AprsError::InvalidCoordinates)?;
    let bytes: [u8; 8] = bytes_slice
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DD MM . HH H/S   (indices 0..7, hemisphere at 7)
    let field = bytes.get(..7).ok_or(AprsError::InvalidCoordinates)?;
    let (digits, ambiguity) = unmask_coord_digits(field, 4)?;
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let deg_str = text.get(..2).ok_or(AprsError::InvalidCoordinates)?;
    let min_str = text.get(2..7).ok_or(AprsError::InvalidCoordinates)?;
    let degrees: f64 = deg_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = min_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = *bytes.get(7).ok_or(AprsError::InvalidCoordinates)?;

    let mut lat = degrees + minutes / 60.0;
    if hemisphere == b'S' {
        lat = -lat;
    } else if hemisphere != b'N' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lat, ambiguity))
}

/// Parse APRS longitude from the standard `DDDMM.HH[E/W]` format.
fn parse_aprs_longitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    let bytes_slice = s.get(..9).ok_or(AprsError::InvalidCoordinates)?;
    let bytes: [u8; 9] = bytes_slice
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DDD MM . HH E/W  (indices 0..8, hemisphere at 8)
    let field = bytes.get(..8).ok_or(AprsError::InvalidCoordinates)?;
    let (digits, ambiguity) = unmask_coord_digits(field, 5)?;
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let deg_str = text.get(..3).ok_or(AprsError::InvalidCoordinates)?;
    let min_str = text.get(3..8).ok_or(AprsError::InvalidCoordinates)?;
    let degrees: f64 = deg_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = min_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = *bytes.get(8).ok_or(AprsError::InvalidCoordinates)?;

    let mut lon = degrees + minutes / 60.0;
    if hemisphere == b'W' {
        lon = -lon;
    } else if hemisphere != b'E' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lon, ambiguity))
}

/// Replace space-masked digits with `'0'` and return the masked-count
/// alongside the rebuilt byte sequence. `dot_idx` is the index of the
/// literal `.` in the field (4 for latitude, 5 for longitude).
fn unmask_coord_digits(
    field: &[u8],
    dot_idx: usize,
) -> Result<([u8; 8], PositionAmbiguity), AprsError> {
    if field.len() > 8 {
        return Err(AprsError::InvalidCoordinates);
    }
    let dot_byte = *field.get(dot_idx).ok_or(AprsError::InvalidCoordinates)?;
    if dot_byte != b'.' {
        return Err(AprsError::InvalidCoordinates);
    }
    // Ambiguity is counted by walking the mask-eligible positions from
    // rightmost back to the start until we stop seeing spaces.
    // Mask order (rightmost first): HH tens, HH ones, MM ones, MM tens
    let mask_order: [usize; 4] = if dot_idx == 4 {
        [6, 5, 3, 2] // lat: HH(6,5), MM(3,2)
    } else {
        [7, 6, 4, 3] // lon: HH(7,6), MM(4,3)
    };
    let mut count: u8 = 0;
    for &pos in &mask_order {
        if field.get(pos) == Some(&b' ') {
            count += 1;
        } else {
            break;
        }
    }
    // Build output buffer.
    let mut out = [b'0'; 8];
    if let Some(dst) = out.get_mut(..field.len()) {
        dst.copy_from_slice(field);
    }
    let masked = mask_order.get(..count as usize).unwrap_or(&[]);
    for pos in masked {
        if let Some(slot) = out.get_mut(*pos) {
            *slot = b'0';
        }
    }
    // Also fail if we see a space at a non-maskable position (outside
    // the trailing run).
    for (i, &b) in field.iter().enumerate() {
        if b == b' ' && !masked.contains(&i) {
            return Err(AprsError::InvalidCoordinates);
        }
    }
    let ambiguity = match count {
        0 => PositionAmbiguity::None,
        1 => PositionAmbiguity::OneDigit,
        2 => PositionAmbiguity::TwoDigits,
        3 => PositionAmbiguity::ThreeDigits,
        _ => PositionAmbiguity::FourDigits,
    };
    Ok((out, ambiguity))
}

/// Parse an APRS position report from an AX.25 information field.
///
/// Supports three APRS position formats (per APRS101.PDF chapters 8-9):
/// - **Uncompressed**: `!`/`=`/`/`/`@` with ASCII lat/lon (`DDMM.HH`)
/// - **Compressed**: `!`/`=`/`/`/`@` with base-91 encoded lat/lon (13 bytes)
///
/// For **Mic-E** positions (`` ` ``/`'`), use
/// [`crate::mic_e::parse_mice_position`] which also requires the AX.25
/// destination address.
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or coordinates are invalid.
pub fn parse_aprs_position(info: &[u8]) -> Result<AprsPosition, AprsError> {
    let data_type = *info.first().ok_or(AprsError::InvalidFormat)?;
    let body = match data_type {
        // Position without timestamp: ! or =
        b'!' | b'=' => info.get(1..).ok_or(AprsError::InvalidFormat)?,
        // Position with timestamp: / or @
        // Timestamp is 7 characters after the type byte
        b'/' | b'@' => info.get(8..).ok_or(AprsError::InvalidFormat)?,
        _ => return Err(AprsError::InvalidFormat),
    };

    let first = *body.first().ok_or(AprsError::InvalidFormat)?;
    // Detect compressed vs uncompressed: if the first byte is a digit (0-9),
    // it's uncompressed latitude. Otherwise it's a compressed symbol table char.
    if first.is_ascii_digit() {
        parse_uncompressed_body(body)
    } else {
        parse_compressed_body(body)
    }
}

/// Parse uncompressed APRS position body.
///
/// Format: `lat(8) sym_table(1) lon(9) sym_code(1) [comment]` = 19+ bytes.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the body is shorter than 19
/// bytes or [`AprsError::InvalidCoordinates`] if the latitude or
/// longitude fields are malformed.
pub fn parse_uncompressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    let lat_slice = body.get(..8).ok_or(AprsError::InvalidFormat)?;
    let (latitude, lat_ambig) = parse_aprs_latitude(lat_slice)?;
    let symbol_table = *body.get(8).ok_or(AprsError::InvalidFormat)? as char;
    let lon_slice = body.get(9..18).ok_or(AprsError::InvalidFormat)?;
    let (longitude, lon_ambig) = parse_aprs_longitude(lon_slice)?;
    let symbol_code = *body.get(18).ok_or(AprsError::InvalidFormat)? as char;
    // A position's ambiguity is the maximum of the two component
    // ambiguities — whichever field was masked more aggressively wins.
    let ambiguity = std::cmp::max_by_key(lat_ambig, lon_ambig, |a| match a {
        PositionAmbiguity::None => 0,
        PositionAmbiguity::OneDigit => 1,
        PositionAmbiguity::TwoDigits => 2,
        PositionAmbiguity::ThreeDigits => 3,
        PositionAmbiguity::FourDigits => 4,
    });

    let comment = body.get(19..).map_or_else(String::new, |rest| {
        String::from_utf8_lossy(rest).into_owned()
    });

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    // If the comment had a CSE/SPD extension, surface it on speed/course
    // too so callers that only read those fields see the data.
    let (speed_knots, course_degrees) = match extensions.course_speed {
        Some((course, speed)) => (Some(speed), Some(course)),
        None => (None, None),
    };
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions,
        mice_message: None,
        mice_altitude_m: None,
        ambiguity,
    })
}

/// Parse compressed APRS position body (APRS101.PDF Chapter 9).
///
/// Format: `sym_table(1) YYYY(4) XXXX(4) sym_code(1) cs(1) s(1) t(1)` = 13 bytes.
/// YYYY and XXXX are base-91 encoded (each byte = ASCII 33-124, value = byte - 33).
///
/// Latitude:  `90 - (YYYY / 380926.0)` degrees
/// Longitude: `-180 + (XXXX / 190463.0)` degrees
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the body is shorter than 13
/// bytes or [`AprsError::InvalidCoordinates`] if the base-91 lat/lon
/// fields contain bytes outside the `33..=124` range.
pub fn parse_compressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    // Require the full 13-byte compressed body upfront.
    let header = body.get(..13).ok_or(AprsError::InvalidFormat)?;
    let symbol_table = *header.first().ok_or(AprsError::InvalidFormat)? as char;
    let lat_bytes = header.get(1..5).ok_or(AprsError::InvalidFormat)?;
    let lon_bytes = header.get(5..9).ok_or(AprsError::InvalidFormat)?;
    let lat_val = decode_base91_4(lat_bytes)?;
    let lon_val = decode_base91_4(lon_bytes)?;
    let symbol_code = *header.get(9).ok_or(AprsError::InvalidFormat)? as char;

    let latitude = 90.0 - f64::from(lat_val) / 380_926.0;
    let longitude = -180.0 + f64::from(lon_val) / 190_463.0;

    // Decode the 3-byte cs/s/t tail per APRS 1.0.1 §9.
    let cs_byte = *header.get(10).ok_or(AprsError::InvalidFormat)?;
    let s_byte = *header.get(11).ok_or(AprsError::InvalidFormat)?;
    let t_byte = *header.get(12).ok_or(AprsError::InvalidFormat)?;
    let (compressed_altitude_ft, compressed_course_speed) =
        decode_compressed_tail(cs_byte, s_byte, t_byte);

    let comment = body.get(13..).map_or_else(String::new, |rest| {
        String::from_utf8_lossy(rest).into_owned()
    });

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    // Surface course/speed into the direct fields too.
    let (speed_knots, course_degrees) =
        compressed_course_speed.map_or((None, None), |(course, speed)| (Some(speed), Some(course)));
    let final_extensions = if let Some(alt) = compressed_altitude_ft {
        AprsDataExtension {
            altitude_ft: Some(alt),
            ..extensions
        }
    } else {
        extensions
    };
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions: final_extensions,
        mice_message: None,
        mice_altitude_m: None,
        // Compressed positions do not use APRS §8.1.6 ambiguity.
        ambiguity: PositionAmbiguity::None,
    })
}

/// Decode the 3-byte `cs`/`s`/`t` compression tail of a compressed APRS
/// position report per APRS 1.0.1 §9 Table 10.
///
/// Returns `(altitude_ft, course_speed)` where either may be `None`.
fn decode_compressed_tail(cs: u8, s: u8, t: u8) -> (Option<i32>, Option<(u16, u16)>) {
    // Space in the `cs` column means "no data."
    if cs == b' ' {
        return (None, None);
    }
    // The `t` byte minus 33 gives a 6-bit compression type value. Bits
    // 3-4 (0x18) select the semantic meaning of `cs`/`s`.
    let t_val = t.saturating_sub(33);
    let type_bits = (t_val >> 3) & 0x03;
    match type_bits {
        // 0b00 / 0b01: course (c) + speed (s). Course is (cs - 33) * 4
        // degrees. Speed is 1.08^(s - 33) - 1 knots.
        0 | 1 => {
            let c = cs.saturating_sub(33);
            let s_val = s.saturating_sub(33);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let speed_knots = (1.08_f64.powi(i32::from(s_val)) - 1.0).round() as u16;
            let course_deg = u16::from(c) * 4;
            // Course 0 == "no data" per spec convention.
            if course_deg == 0 && speed_knots == 0 {
                (None, None)
            } else {
                (None, Some((course_deg, speed_knots)))
            }
        }
        // 0b10: altitude. cs,s = base-91 two-char altitude, value =
        // 1.002^((cs-33)*91 + (s-33)) feet.
        2 => {
            let c = i32::from(cs.saturating_sub(33));
            let s_val = i32::from(s.saturating_sub(33));
            let exponent = c * 91 + s_val;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let alt_ft = 1.002_f64.powi(exponent).round() as i32;
            (Some(alt_ft), None)
        }
        // 0b11 (range): not currently surfaced.
        _ => (None, None),
    }
}

/// Decode a 4-byte base-91 value.
///
/// Each byte is in the ASCII range 33-124. The value is:
/// `b[0]*91^3 + b[1]*91^2 + b[2]*91 + b[3]`
///
/// # Errors
///
/// Returns [`AprsError::InvalidCoordinates`] if `bytes` is shorter than
/// 4 bytes or any byte is outside the `33..=124` base-91 range.
pub fn decode_base91_4(bytes: &[u8]) -> Result<u32, AprsError> {
    let window = bytes.get(..4).ok_or(AprsError::InvalidCoordinates)?;
    let mut val: u32 = 0;
    for &b in window {
        if !(33..=124).contains(&b) {
            return Err(AprsError::InvalidCoordinates);
        }
        val = val * 91 + u32::from(b - 33);
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- APRS position tests ----

    #[test]
    fn parse_aprs_position_no_timestamp() -> TestResult {
        let info = b"!4903.50N/07201.75W-Test comment";
        let pos = parse_aprs_position(info)?;
        // 49 degrees 3.50 minutes N = 49.058333...
        assert!(
            (pos.latitude - 49.058_333).abs() < 0.001,
            "lat={}",
            pos.latitude
        );
        // 72 degrees 1.75 minutes W = -72.029166...
        assert!(
            (pos.longitude - (-72.029_166)).abs() < 0.001,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '-');
        assert_eq!(pos.comment, "Test comment");
        Ok(())
    }

    #[test]
    fn parse_aprs_position_with_timestamp() -> TestResult {
        // '@' type with DHM timestamp "092345z"
        let info = b"@092345z4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert!((pos.latitude - 49.058_333).abs() < 0.001, "lat check");
        assert!((pos.longitude - (-72.029_166)).abs() < 0.001, "lon check");
        Ok(())
    }

    #[test]
    fn parse_aprs_position_south_east() -> TestResult {
        let info = b"!3356.65S/15113.72E>";
        let pos = parse_aprs_position(info)?;
        assert!(
            pos.latitude < 0.0,
            "expected South, got lat={}",
            pos.latitude
        );
        assert!(
            pos.longitude > 0.0,
            "expected East, got lon={}",
            pos.longitude
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_messaging_enabled() -> TestResult {
        let info = b"=4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert!((pos.latitude - 49.058_333).abs() < 0.001, "lat check");
        Ok(())
    }

    #[test]
    fn parse_aprs_position_invalid_type() {
        let info = b"X4903.50N/07201.75W-";
        assert!(
            parse_aprs_position(info).is_err(),
            "expected error for invalid type",
        );
    }

    #[test]
    fn parse_aprs_position_too_short() {
        assert!(
            parse_aprs_position(b"!short").is_err(),
            "expected error for short input",
        );
    }

    #[test]
    fn parse_aprs_position_empty() {
        assert!(
            parse_aprs_position(b"").is_err(),
            "expected error for empty"
        );
    }

    // ---- APRS compressed position tests ----

    #[test]
    fn parse_aprs_compressed_position() -> TestResult {
        // Use computed example with known values.
        // lat_val = 3493929 → lat = 90 - 3493929/380926 = 80.828
        //   bytes: ('%', 'Z', 't', 'l')
        // lon_val = 4567890 → lon = -180 + 4567890/190463 = -156.018
        //   bytes: (''', '&', 'X', 'W')
        let body: &[u8] = b"/%Ztl'&XW> sT";
        let mut info = vec![b'!'];
        info.extend_from_slice(body);

        let pos = parse_aprs_position(&info)?;
        assert!((pos.latitude - 80.828).abs() < 0.01, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-156.018)).abs() < 0.01,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '>');
        Ok(())
    }

    #[test]
    fn parse_aprs_compressed_with_timestamp() -> TestResult {
        let mut info = Vec::new();
        info.push(b'@');
        info.extend_from_slice(b"092345z"); // 7-char timestamp
        info.extend_from_slice(b"/%Ztl'&XW> sT"); // compressed body
        let pos = parse_aprs_position(&info)?;
        assert!((pos.latitude - 80.828).abs() < 0.01, "lat check");
        Ok(())
    }

    #[test]
    fn parse_aprs_compressed_too_short() {
        let info = b"!/short";
        assert!(parse_aprs_position(info).is_err(), "too-short compressed");
    }

    #[test]
    fn base91_decode_zero() -> TestResult {
        assert_eq!(decode_base91_4(b"!!!!")?, 0);
        Ok(())
    }

    #[test]
    fn base91_decode_max() -> TestResult {
        let val = decode_base91_4(b"||||")?;
        let expected = 91_u32 * 753_571 + 91 * 8281 + 91 * 91 + 91;
        assert_eq!(val, expected);
        Ok(())
    }

    #[test]
    fn base91_decode_invalid_char() {
        assert!(
            decode_base91_4(b" !!!").is_err(),
            "space is below valid range"
        );
    }

    #[test]
    fn parse_position_with_one_digit_ambiguity() -> TestResult {
        let info = b"!4903.5 N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity, PositionAmbiguity::OneDigit);
        assert!((pos.latitude - 49.0583).abs() < 0.001, "lat check");
        Ok(())
    }

    #[test]
    fn parse_position_with_two_digit_ambiguity() -> TestResult {
        let info = b"!4903.  N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity, PositionAmbiguity::TwoDigits);
        Ok(())
    }

    #[test]
    fn parse_position_with_four_digit_ambiguity() -> TestResult {
        let info = b"!49  .  N/072  .  W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity, PositionAmbiguity::FourDigits);
        Ok(())
    }

    #[test]
    fn parse_position_full_precision_has_no_ambiguity() -> TestResult {
        let info = b"!4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity, PositionAmbiguity::None);
        Ok(())
    }

    #[test]
    fn parse_position_populates_extensions_from_comment() -> TestResult {
        let info = b"!3515.00N/09745.00W>088/036/A=001234hello";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.extensions.course_speed, Some((88, 36)));
        assert_eq!(pos.extensions.altitude_ft, Some(1234));
        assert_eq!(pos.speed_knots, Some(36));
        assert_eq!(pos.course_degrees, Some(88));
        Ok(())
    }

    #[test]
    fn parse_position_embedded_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W_090/010g015t072r001P020h55b10135";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.symbol_code, '_');
        let wx = pos.weather.ok_or("embedded weather missing")?;
        assert_eq!(wx.wind_direction, Some(90));
        assert_eq!(wx.wind_speed, Some(10));
        assert_eq!(wx.wind_gust, Some(15));
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.rain_1h, Some(1));
        assert_eq!(wx.rain_since_midnight, Some(20));
        assert_eq!(wx.humidity, Some(55));
        assert_eq!(wx.pressure, Some(10135));
        Ok(())
    }

    #[test]
    fn parse_position_without_weather_symbol_has_no_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W>mobile comment";
        let pos = parse_aprs_position(info)?;
        assert!(pos.weather.is_none(), "no weather expected");
        Ok(())
    }

    #[test]
    fn parse_position_weather_symbol_bad_format_has_no_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W_hello";
        let pos = parse_aprs_position(info)?;
        assert!(pos.weather.is_none(), "bad format → no weather");
        Ok(())
    }
}
