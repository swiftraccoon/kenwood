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
/// All fields are optional; weather stations may report any subset.
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
#[must_use]
pub fn extract_position_weather(symbol_code: char, comment: &str) -> Option<AprsWeather> {
    if symbol_code != '_' {
        return None;
    }
    let bytes = comment.as_bytes();
    let header = bytes.get(..7)?;
    if header.get(3) != Some(&b'/') {
        return None;
    }
    // Per APRS 1.0.1 §12.2 (p.64), wind direction/speed may be reported
    // as dots or spaces when the station has no wind sensor (spec example
    // `_10090556c...s...g...t...P012Jim`). A `.`/space placeholder in the
    // `DDD/SSS` extension means "no data" for that field, NOT a malformed
    // report: the remaining weather fields (gust/temp/humidity/pressure)
    // must still be parsed. `parse_weather_value` maps an all-dots/spaces
    // run to `None`; any other non-digit content is genuinely invalid and
    // aborts the whole parse (a position-with-`_`-symbol comment whose
    // first 7 bytes are not a `DDD/SSS` extension is not a weather report).
    let dir_bytes = header.get(..3)?;
    let spd_bytes = header.get(4..7)?;
    let wind_dir = parse_wind_field(dir_bytes)?.into_option();
    let wind_spd = parse_wind_field(spd_bytes)?.into_option();
    let tail = bytes.get(7..)?;
    let mut wx = parse_weather_fields(tail);
    wx.wind_direction = wind_dir;
    wx.wind_speed = wind_spd;
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
            // Unknown byte: assume start of comment / type suffix.
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

/// A `DDD` / `SSS` wind-direction / wind-speed field, classified into the
/// two non-error outcomes the spec permits.
///
/// The error case (the bytes are neither a number nor a dots/spaces
/// placeholder, so the comment is not a `DDD/SSS` extension at all) is
/// modelled by the surrounding [`Option`] from [`parse_wind_field`], so
/// the caller can use `?` to abort the whole parse.
enum WindField {
    /// All ASCII digits, parsed and range-checked to a `u16` degree/mph
    /// value.
    Value(u16),
    /// All dots or spaces: the spec "no data" sentinel (APRS 1.0.1 §12.2
    /// p.64). The field is unknown but the surrounding report is valid and
    /// the remaining weather fields must still be parsed.
    Unknown,
}

impl WindField {
    /// Map to the `Option<u16>` representation used by [`AprsWeather`]'s
    /// wind fields (`None` for the spec "no data" sentinel).
    const fn into_option(self) -> Option<u16> {
        match self {
            Self::Value(v) => Some(v),
            Self::Unknown => None,
        }
    }
}

/// Classify a fixed-width `DDD` / `SSS` extension field.
///
/// Returns `None` (so the caller aborts via `?`) when the bytes are
/// neither all digits nor all dots/spaces; such a prefix is not a
/// `DDD/SSS` extension and the comment is not a weather report. A digit
/// run that parses but overflows `u16` (an impossible 4+ digit value in a
/// 3-byte field, but defended anyway) also yields `None`.
fn parse_wind_field(bytes: &[u8]) -> Option<WindField> {
    if bytes.iter().all(u8::is_ascii_digit) {
        let s = std::str::from_utf8(bytes).ok()?;
        let raw: i32 = s.parse().ok()?;
        return Some(WindField::Value(convert_u16(raw)?));
    }
    if bytes.iter().all(|&b| b == b'.' || b == b' ') {
        return Some(WindField::Unknown);
    }
    None
}

/// Lossless narrowing from `i32` to `u16` for weather values.
fn convert_u16(v: i32) -> Option<u16> {
    u16::try_from(v).ok()
}

/// Lossless narrowing from `i32` to `i16` for signed weather values.
fn convert_i16(v: i32) -> Option<i16> {
    i16::try_from(v).ok()
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
        // Temperature only; other fields omitted entirely.
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

    #[test]
    fn extract_position_weather_dotted_wind_keeps_other_fields() -> TestResult {
        // APRS 1.0.1 §12.2 p.64: a station with no wind sensor reports the
        // `DDD/SSS` extension as dots. The old guard returned None for the
        // whole report; the fix surfaces wind=None and parses the rest.
        let wx = extract_position_weather('_', ".../...g005t072h50b10132")
            .ok_or("dotted wind must still parse as a weather report")?;
        assert_eq!(wx.wind_direction, None);
        assert_eq!(wx.wind_speed, None);
        assert_eq!(wx.wind_gust, Some(5));
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.humidity, Some(50));
        assert_eq!(wx.pressure, Some(10132));
        Ok(())
    }

    #[test]
    fn extract_position_weather_real_wind_still_parses() -> TestResult {
        // Spec p.65 example `_220/004g005t077...` (digits, not dots): the
        // populated path must keep working alongside the dotted path.
        let wx = extract_position_weather('_', "220/004g005t077r000p000P000h50b09900")
            .ok_or("digit wind must parse")?;
        assert_eq!(wx.wind_direction, Some(220));
        assert_eq!(wx.wind_speed, Some(4));
        assert_eq!(wx.wind_gust, Some(5));
        assert_eq!(wx.temperature, Some(77));
        assert_eq!(wx.pressure, Some(9900));
        Ok(())
    }

    #[test]
    fn extract_position_weather_rejects_non_extension_comment() {
        // A `_`-symbol comment whose first 7 bytes are neither digits nor
        // a dots placeholder is not a `DDD/SSS` extension at all.
        assert_eq!(extract_position_weather('_', "abc/defghi"), None);
    }

    #[test]
    fn parse_weather_fully_dotted_spec_example() -> TestResult {
        // Spec p.64 example `_10090556c...s...g...t...P012Jim`: every
        // mandatory field is dots except rain-since-midnight. The non-wind
        // field (P012) must survive; nothing is lost because the wind
        // fields are unknown.
        let wx = parse_aprs_weather_positionless(b"_10090556c...s...g...t...P012Jim")?;
        assert_eq!(wx.wind_direction, None);
        assert_eq!(wx.wind_speed, None);
        assert_eq!(wx.wind_gust, None);
        assert_eq!(wx.temperature, None);
        assert_eq!(wx.rain_since_midnight, Some(12));
        Ok(())
    }
}
