//! APRS weather reports (APRS 1.0.1 ch. 12).
//!
//! Covers both standalone positionless weather frames (data type `_`)
//! and weather data embedded in a position report when the symbol code
//! is `_` (weather station).

use crate::error::AprsError;

/// An APRS weather report.
///
/// Weather data can be embedded in a position report or sent as a
/// standalone positionless weather report (data type `_`). The TH-D75
/// displays weather station data in the station list.
///
/// All fields are optional — weather stations may report any subset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AprsWeather {
    /// Wind direction in degrees (0-360).
    pub wind_direction: Option<u16>,
    /// Wind speed in mph.
    pub wind_speed: Option<u16>,
    /// Wind gust in mph (peak in last 5 minutes).
    pub wind_gust: Option<u16>,
    /// Temperature in degrees Fahrenheit.
    pub temperature: Option<i16>,
    /// Rainfall in last hour (hundredths of an inch).
    pub rain_1h: Option<u16>,
    /// Rainfall in last 24 hours (hundredths of an inch).
    pub rain_24h: Option<u16>,
    /// Rainfall since midnight (hundredths of an inch).
    pub rain_since_midnight: Option<u16>,
    /// Humidity in percent (1-100). Raw APRS `00` is converted to 100.
    pub humidity: Option<u8>,
    /// Barometric pressure in tenths of millibars/hPa.
    pub pressure: Option<u32>,
}

/// Try to extract weather data embedded in a position report's comment.
///
/// Per APRS 1.0.1 §12.1, a "complete weather report" is a position report
/// with symbol code `_` (weather station) whose comment begins with the
/// CSE/SPD extension format `DDD/SSS` encoding wind direction and speed,
/// followed by the remaining weather fields (`gGGG tTTT rRRR …`) in the
/// standard order.
///
/// Returns `None` if the symbol is not `_` or the comment does not start
/// with a valid `DDD/SSS` extension.
pub fn extract_position_weather(symbol_code: char, comment: &str) -> Option<AprsWeather> {
    if symbol_code != '_' {
        return None;
    }
    let bytes = comment.as_bytes();
    let header = bytes.get(..7)?;
    if header.get(3) != Some(&b'/') {
        return None;
    }
    let dir_bytes = header.get(..3)?;
    let spd_bytes = header.get(4..7)?;
    if !dir_bytes.iter().all(u8::is_ascii_digit) || !spd_bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let wind_dir: u16 = comment.get(..3)?.parse().ok()?;
    let wind_spd: u16 = comment.get(4..7)?.parse().ok()?;
    let tail = bytes.get(7..)?;
    let mut wx = parse_weather_fields(tail);
    wx.wind_direction = Some(wind_dir);
    wx.wind_speed = Some(wind_spd);
    Some(wx)
}

/// Parse a positionless APRS weather report (`_MMDDHHMMdata`).
///
/// Weather data uses single-letter field tags followed by fixed-width
/// numeric values. Common fields:
/// - `c` = wind direction (3 digits, degrees)
/// - `s` = wind speed (3 digits, mph)
/// - `g` = gust (3 digits, mph)
/// - `t` = temperature (3 digits, Fahrenheit, may be negative)
/// - `r` = rain last hour (3 digits, hundredths of inch)
/// - `p` = rain last 24h (3 digits, hundredths of inch)
/// - `P` = rain since midnight (3 digits, hundredths of inch)
/// - `h` = humidity (2 digits, 00=100%)
/// - `b` = barometric pressure (5 digits, tenths of mbar)
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the info field does not begin
/// with the `_` data type identifier.
pub fn parse_aprs_weather_positionless(info: &[u8]) -> Result<AprsWeather, AprsError> {
    if info.first() != Some(&b'_') {
        return Err(AprsError::InvalidFormat);
    }
    // Skip _ and 8-char timestamp (MMDDHHMM)
    let data = info.get(9..).unwrap_or(&[]);
    Ok(parse_weather_fields(data))
}

/// Parse APRS weather data fields from a byte slice.
///
/// Per APRS 1.0.1 §12.2, weather fields are a contiguous sequence of
/// `<tag><value>` pairs in a **fixed order** (wind direction, wind speed,
/// gust, temperature, rain 1h, rain 24h, rain since midnight, humidity,
/// pressure, luminosity). Each field is optional and, if present, uses a
/// fixed-width decimal value. A value of all dots or spaces means the
/// station has no data for that field.
///
/// The parser walks the buffer from the start, consumes a known tag +
/// value pair, and advances. It stops on the first unknown byte, leaving
/// any trailing comment / station-type suffix alone.
///
/// This is strictly more correct than a `find()`-based scan, which would
/// false-match tag letters appearing inside comment text (e.g. `"canada"`
/// matching `c` for wind direction).
///
/// Private by design: callers outside this crate should use
/// [`parse_aprs_weather_positionless`] (which validates the leading
/// `_` + 8-byte timestamp and then delegates here) to avoid mistaking
/// non-weather bytes for weather data.
fn parse_weather_fields(data: &[u8]) -> AprsWeather {
    let mut wx = AprsWeather::default();
    let mut i = 0;
    while let Some(&tag) = data.get(i) {
        let width = match tag {
            b'c' | b's' | b'g' | b't' | b'r' | b'p' | b'P' | b'L' | b'l' => 3,
            b'h' => 2,
            b'b' => 5,
            // Unknown byte — assume start of comment / type suffix.
            _ => break,
        };
        let Some(val_bytes) = data.get(i + 1..i + 1 + width) else {
            break;
        };
        let parsed_i32 = parse_weather_value(val_bytes);
        match tag {
            b'c' => {
                // Wind direction: 000 is the "true North / no data"
                // convention; most stations encode 360 as 000.
                wx.wind_direction = parsed_i32.and_then(convert_u16);
            }
            b's' => wx.wind_speed = parsed_i32.and_then(convert_u16),
            b'g' => wx.wind_gust = parsed_i32.and_then(convert_u16),
            b't' => wx.temperature = parsed_i32.and_then(convert_i16),
            b'r' => wx.rain_1h = parsed_i32.and_then(convert_u16),
            b'p' => wx.rain_24h = parsed_i32.and_then(convert_u16),
            b'P' => wx.rain_since_midnight = parsed_i32.and_then(convert_u16),
            b'h' => {
                // APRS encodes humidity 100% as "00".
                wx.humidity = parsed_i32.and_then(|v| {
                    if v == 0 {
                        Some(100)
                    } else {
                        u8::try_from(v).ok()
                    }
                });
            }
            b'b' => wx.pressure = parsed_i32.and_then(|v| u32::try_from(v).ok()),
            // Luminosity (L/l): not yet represented in AprsWeather.
            b'L' | b'l' => {}
            // The match above ensures only the tag bytes we set a width
            // for reach here; other bytes cause the loop to break above.
            _ => break,
        }
        i += 1 + width;
    }
    wx
}

/// Parse a fixed-width weather field value. Returns `None` if the bytes
/// are a "no data" placeholder (dots or spaces) or unparseable.
fn parse_weather_value(bytes: &[u8]) -> Option<i32> {
    if bytes.iter().all(|&b| b == b'.' || b == b' ') {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    s.trim().parse().ok()
}

/// Lossless widening from `i32` to `u16` for weather values.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "The function checks `v < 0 || v > u16::MAX as i32` before casting, so the \
              `v as u16` in the else branch is always lossless. `cast_possible_truncation` \
              and `cast_sign_loss` fire because clippy doesn't follow the surrounding \
              `if` guard to prove the range. A `u16::try_from(v).ok()` rewrite would \
              eliminate both the casts and the suppressions — tracked as a fix-the-code \
              candidate."
)]
const fn convert_u16(v: i32) -> Option<u16> {
    if v < 0 || v > u16::MAX as i32 {
        None
    } else {
        Some(v as u16)
    }
}

/// Lossless widening from `i32` to `i16` for signed weather values.
#[expect(
    clippy::cast_possible_truncation,
    reason = "The function checks `v < i16::MIN as i32 || v > i16::MAX as i32` before \
              casting, so the `v as i16` in the else branch is always lossless. \
              `cast_possible_truncation` fires because clippy doesn't follow the \
              surrounding guard to prove the range. A `i16::try_from(v).ok()` rewrite \
              would eliminate the cast — tracked as a fix-the-code candidate."
)]
const fn convert_i16(v: i32) -> Option<i16> {
    if v < i16::MIN as i32 || v > i16::MAX as i32 {
        None
    } else {
        Some(v as i16)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_weather_positionless_full() -> TestResult {
        let info = b"_01011234c180s005g010t075r001p010P020h55b10135";
        let wx = parse_aprs_weather_positionless(info)?;
        assert_eq!(wx.wind_direction, Some(180));
        assert_eq!(wx.wind_speed, Some(5));
        assert_eq!(wx.wind_gust, Some(10));
        assert_eq!(wx.temperature, Some(75));
        assert_eq!(wx.rain_1h, Some(1));
        assert_eq!(wx.rain_24h, Some(10));
        assert_eq!(wx.rain_since_midnight, Some(20));
        assert_eq!(wx.humidity, Some(55));
        assert_eq!(wx.pressure, Some(10135));
        Ok(())
    }

    #[test]
    fn parse_weather_missing_fields() -> TestResult {
        let info = b"_01011234c...s...t072";
        let wx = parse_aprs_weather_positionless(info)?;
        assert_eq!(wx.wind_direction, None); // dots = missing
        assert_eq!(wx.wind_speed, None);
        assert_eq!(wx.temperature, Some(72));
        Ok(())
    }

    #[test]
    fn parse_weather_humidity_zero_means_100() -> TestResult {
        let info = b"_01011234h00";
        let wx = parse_aprs_weather_positionless(info)?;
        assert_eq!(wx.humidity, Some(100));
        Ok(())
    }

    #[test]
    fn parse_weather_stops_on_comment_text() -> TestResult {
        // Regression: the old find('c')-based parser would match 'c' in
        // the word "canada" inside a comment. The new position-based
        // parser stops on the first unknown byte.
        let info = b"_01011234t072canada";
        let wx = parse_aprs_weather_positionless(info)?;
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.wind_direction, None); // must NOT be Some(nad)
        Ok(())
    }

    #[test]
    fn parse_weather_fields_in_order_with_gaps() {
        // Temperature only — other fields omitted entirely.
        let wx = parse_weather_fields(b"t072");
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.wind_direction, None);
    }

    #[test]
    fn parse_weather_rejects_trailing_garbage() {
        // The old parser would still find 'b' anywhere. The new parser
        // stops at the first unknown byte.
        let wx = parse_weather_fields(b"t072 b is not pressure");
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.pressure, None);
    }
}
