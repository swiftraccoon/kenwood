//! APRS weather reports (APRS 1.0.1 ch. 12).
//!
//! Covers both standalone positionless weather frames (data type `_`)
//! and weather data embedded in a position report when the symbol code
//! is `_` (weather station).

use std::fmt;

use thiserror::Error;

use crate::error::AprsError;
use crate::packet::AprsWeatherTimestamp;
use crate::text::{WeatherComment, decode_wire_ascii};
use crate::units::Fahrenheit;

/// Errors produced while constructing typed APRS weather values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum WeatherValueError {
    /// Wind direction exceeded the APRS degree range.
    #[error("APRS wind direction must be 0..=360 degrees (got {value})")]
    WindDirectionOutOfRange {
        /// Rejected direction in degrees.
        value: u16,
    },
    /// A three-digit weather measurement exceeded its wire field.
    #[error("APRS three-digit weather value must be 0..=999 (got {value})")]
    ThreeDigitValueOutOfRange {
        /// Rejected measurement.
        value: u16,
    },
    /// Humidity was outside the representable percentage range.
    #[error("APRS humidity must be 1..=100 percent (got {value})")]
    HumidityOutOfRange {
        /// Rejected percentage.
        value: u8,
    },
    /// Barometric pressure exceeded its five-digit wire field.
    #[error("APRS barometric pressure must be 0..=99999 tenths of hPa (got {value})")]
    PressureOutOfRange {
        /// Rejected pressure in tenths of hPa.
        value: u32,
    },
    /// Luminosity exceeded the combined `L`/`l` representation.
    #[error("APRS luminosity must be 0..=1999 watts per square metre (got {value})")]
    LuminosityOutOfRange {
        /// Rejected luminosity in watts per square metre.
        value: u16,
    },
}

/// Wind direction in APRS's `000..=360` degree range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindDirection(u16);

impl WindDirection {
    /// Minimum representable direction.
    pub const MIN: u16 = 0;
    /// Maximum representable direction.
    pub const MAX: u16 = 360;

    /// Create a wind direction.
    ///
    /// # Errors
    ///
    /// Returns [`WeatherValueError::WindDirectionOutOfRange`] above 360.
    pub const fn new(value: u16) -> Result<Self, WeatherValueError> {
        if value > Self::MAX {
            return Err(WeatherValueError::WindDirectionOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the direction in degrees.
    #[must_use]
    pub const fn degrees(self) -> u16 {
        self.0
    }
}

/// A non-negative APRS weather measurement carried in three decimal bytes.
///
/// This representation is shared by wind speed, gust, and the three rainfall
/// totals. Their units remain documented on the containing [`AprsWeather`]
/// accessor while this type enforces the common `000..=999` wire invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreeDigitWeatherValue(u16);

impl ThreeDigitWeatherValue {
    /// Minimum representable value.
    pub const MIN: u16 = 0;
    /// Maximum representable value.
    pub const MAX: u16 = 999;

    /// Create a three-digit weather value.
    ///
    /// # Errors
    ///
    /// Returns [`WeatherValueError::ThreeDigitValueOutOfRange`] above 999.
    pub const fn new(value: u16) -> Result<Self, WeatherValueError> {
        if value > Self::MAX {
            return Err(WeatherValueError::ThreeDigitValueOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the measurement value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// Relative humidity in the APRS range `1..=100` percent.
///
/// One hundred percent is encoded as `00` on air; zero percent has no distinct
/// APRS representation and is rejected rather than being changed to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Humidity(u8);

impl Humidity {
    /// Minimum representable humidity.
    pub const MIN: u8 = 1;
    /// Maximum representable humidity.
    pub const MAX: u8 = 100;

    /// Create a humidity percentage.
    ///
    /// # Errors
    ///
    /// Returns [`WeatherValueError::HumidityOutOfRange`] outside `1..=100`.
    pub const fn new(value: u8) -> Result<Self, WeatherValueError> {
        if value < Self::MIN || value > Self::MAX {
            return Err(WeatherValueError::HumidityOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the humidity percentage.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }
}

/// Barometric pressure in tenths of millibars/hPa (`00000..=99999`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BarometricPressure(u32);

impl BarometricPressure {
    /// Minimum representable pressure.
    pub const MIN: u32 = 0;
    /// Maximum representable pressure.
    pub const MAX: u32 = 99_999;

    /// Create a barometric pressure.
    ///
    /// # Errors
    ///
    /// Returns [`WeatherValueError::PressureOutOfRange`] above 99,999.
    pub const fn new(value: u32) -> Result<Self, WeatherValueError> {
        if value > Self::MAX {
            return Err(WeatherValueError::PressureOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return pressure in tenths of hPa.
    #[must_use]
    pub const fn tenths_hpa(self) -> u32 {
        self.0
    }
}

/// Luminosity in watts per square metre.
///
/// APRS uses `L000..L999` for values below 1000 and `l000..l999` for
/// `1000..=1999`; the lowercase tag contributes the thousands offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Luminosity(u16);

impl Luminosity {
    /// Minimum representable luminosity.
    pub const MIN: u16 = 0;
    /// Maximum representable luminosity across the `L` and `l` forms.
    pub const MAX: u16 = 1_999;

    /// Create a luminosity measurement.
    ///
    /// # Errors
    ///
    /// Returns [`WeatherValueError::LuminosityOutOfRange`] above 1,999.
    pub const fn new(value: u16) -> Result<Self, WeatherValueError> {
        if value > Self::MAX {
            return Err(WeatherValueError::LuminosityOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return luminosity in watts per square metre.
    #[must_use]
    pub const fn watts_per_square_meter(self) -> u16 {
        self.0
    }
}

macro_rules! impl_weather_integer_traits {
    ($type:ident, $integer:ty, $accessor:ident) => {
        impl TryFrom<$integer> for $type {
            type Error = WeatherValueError;

            fn try_from(value: $integer) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$type> for $integer {
            fn from(value: $type) -> Self {
                value.$accessor()
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

impl_weather_integer_traits!(WindDirection, u16, degrees);
impl_weather_integer_traits!(ThreeDigitWeatherValue, u16, value);
impl_weather_integer_traits!(Humidity, u8, percent);
impl_weather_integer_traits!(BarometricPressure, u32, tenths_hpa);
impl_weather_integer_traits!(Luminosity, u16, watts_per_square_meter);

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
    wind_direction: Option<WindDirection>,
    /// Wind speed in mph.
    wind_speed: Option<ThreeDigitWeatherValue>,
    /// Wind gust in mph (peak in last 5 minutes).
    wind_gust: Option<ThreeDigitWeatherValue>,
    /// Temperature in degrees Fahrenheit.
    temperature: Option<Fahrenheit>,
    /// Rainfall in last hour (hundredths of an inch).
    rain_1h: Option<ThreeDigitWeatherValue>,
    /// Rainfall in last 24 hours (hundredths of an inch).
    rain_24h: Option<ThreeDigitWeatherValue>,
    /// Rainfall since midnight (hundredths of an inch).
    rain_since_midnight: Option<ThreeDigitWeatherValue>,
    /// Humidity in percent (1-100). Raw APRS `00` is converted to 100.
    humidity: Option<Humidity>,
    /// Barometric pressure in tenths of millibars/hPa.
    pressure: Option<BarometricPressure>,
    /// Luminosity in watts per square metre.
    luminosity: Option<Luminosity>,
}

impl AprsWeather {
    /// Create an empty report with every measurement absent.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wind_direction: None,
            wind_speed: None,
            wind_gust: None,
            temperature: None,
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: None,
            pressure: None,
            luminosity: None,
        }
    }

    /// Return wind direction in degrees.
    #[must_use]
    pub const fn wind_direction(&self) -> Option<WindDirection> {
        self.wind_direction
    }

    /// Set or clear wind direction.
    pub const fn set_wind_direction(&mut self, value: Option<WindDirection>) {
        self.wind_direction = value;
    }

    /// Return sustained one-minute wind speed in mph.
    #[must_use]
    pub const fn wind_speed(&self) -> Option<ThreeDigitWeatherValue> {
        self.wind_speed
    }

    /// Set or clear sustained wind speed.
    pub const fn set_wind_speed(&mut self, value: Option<ThreeDigitWeatherValue>) {
        self.wind_speed = value;
    }

    /// Return peak five-minute wind gust in mph.
    #[must_use]
    pub const fn wind_gust(&self) -> Option<ThreeDigitWeatherValue> {
        self.wind_gust
    }

    /// Set or clear wind gust.
    pub const fn set_wind_gust(&mut self, value: Option<ThreeDigitWeatherValue>) {
        self.wind_gust = value;
    }

    /// Return temperature in degrees Fahrenheit.
    #[must_use]
    pub const fn temperature(&self) -> Option<Fahrenheit> {
        self.temperature
    }

    /// Set or clear temperature.
    pub const fn set_temperature(&mut self, value: Option<Fahrenheit>) {
        self.temperature = value;
    }

    /// Return rainfall during the last hour in hundredths of an inch.
    #[must_use]
    pub const fn rain_1h(&self) -> Option<ThreeDigitWeatherValue> {
        self.rain_1h
    }

    /// Set or clear last-hour rainfall.
    pub const fn set_rain_1h(&mut self, value: Option<ThreeDigitWeatherValue>) {
        self.rain_1h = value;
    }

    /// Return rainfall during the last 24 hours in hundredths of an inch.
    #[must_use]
    pub const fn rain_24h(&self) -> Option<ThreeDigitWeatherValue> {
        self.rain_24h
    }

    /// Set or clear last-24-hour rainfall.
    pub const fn set_rain_24h(&mut self, value: Option<ThreeDigitWeatherValue>) {
        self.rain_24h = value;
    }

    /// Return rainfall since midnight in hundredths of an inch.
    #[must_use]
    pub const fn rain_since_midnight(&self) -> Option<ThreeDigitWeatherValue> {
        self.rain_since_midnight
    }

    /// Set or clear rainfall since midnight.
    pub const fn set_rain_since_midnight(&mut self, value: Option<ThreeDigitWeatherValue>) {
        self.rain_since_midnight = value;
    }

    /// Return relative humidity.
    #[must_use]
    pub const fn humidity(&self) -> Option<Humidity> {
        self.humidity
    }

    /// Set or clear humidity.
    pub const fn set_humidity(&mut self, value: Option<Humidity>) {
        self.humidity = value;
    }

    /// Return barometric pressure in tenths of hPa.
    #[must_use]
    pub const fn pressure(&self) -> Option<BarometricPressure> {
        self.pressure
    }

    /// Set or clear barometric pressure.
    pub const fn set_pressure(&mut self, value: Option<BarometricPressure>) {
        self.pressure = value;
    }

    /// Return luminosity in watts per square metre.
    #[must_use]
    pub const fn luminosity(&self) -> Option<Luminosity> {
        self.luminosity
    }

    /// Set or clear luminosity.
    pub const fn set_luminosity(&mut self, value: Option<Luminosity>) {
        self.luminosity = value;
    }
}

/// A standalone APRS positionless-weather report (data type `_`).
///
/// Unlike weather embedded in a position report, this wire form always
/// carries an eight-byte [`AprsWeatherTimestamp`]. The wrapper preserves
/// that mandatory timestamp and any trailing station comment instead of
/// reducing the frame to measurements alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsPositionlessWeatherReport {
    /// Required month/day/hour/minute UTC timestamp.
    pub timestamp: AprsWeatherTimestamp,
    /// Decoded weather measurements.
    pub weather: AprsWeather,
    comment: WeatherComment,
}

impl AprsPositionlessWeatherReport {
    /// Create a report without a trailing station comment.
    #[must_use]
    pub fn new(timestamp: AprsWeatherTimestamp, weather: AprsWeather) -> Self {
        Self {
            timestamp,
            weather,
            comment: WeatherComment::default(),
        }
    }

    /// Create a report with an exact validated trailing station comment.
    #[must_use]
    pub const fn with_comment(
        timestamp: AprsWeatherTimestamp,
        weather: AprsWeather,
        comment: WeatherComment,
    ) -> Self {
        Self {
            timestamp,
            weather,
            comment,
        }
    }

    /// Return the trailing station comment or type suffix.
    #[must_use]
    pub fn comment(&self) -> &str {
        self.comment.as_str()
    }

    /// Return the typed trailing comment.
    #[must_use]
    pub const fn comment_value(&self) -> &WeatherComment {
        &self.comment
    }
}

/// Try to extract weather data embedded in a position report's comment.
///
/// Per APRS 1.0.1 §12.1, a "complete weather report" is a position report
/// with symbol code `_` (weather station) whose comment begins with the
/// CSE/SPD extension format `DDD/SSS` encoding wind direction and speed,
/// followed by the remaining weather fields (`gGGG tTTT rRRR …`) in the
/// standard order.
///
/// Returns `Ok(None)` if the symbol is not `_` or the comment does not start
/// with a `DDD/SSS` extension. Once a weather-shaped extension is recognized,
/// malformed, out-of-range, duplicate, or reordered fields are rejected.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] when a recognized weather extension
/// violates its fixed-width field grammar or ordering.
pub fn extract_position_weather(
    symbol_code: char,
    comment: &str,
) -> Result<Option<AprsWeather>, AprsError> {
    if symbol_code != '_' {
        return Ok(None);
    }
    let bytes = comment.as_bytes();
    let Some(header) = bytes.get(..7) else {
        return Ok(None);
    };
    if header.get(3) != Some(&b'/') {
        return Ok(None);
    }
    let dir_bytes = header.get(..3).ok_or(AprsError::InvalidFormat)?;
    let speed_bytes = header.get(4..7).ok_or(AprsError::InvalidFormat)?;
    if !is_unsigned_or_placeholder(dir_bytes) || !is_unsigned_or_placeholder(speed_bytes) {
        return Ok(None);
    }

    let wind_direction = parse_unsigned_field(dir_bytes)?
        .map(|value| {
            let value = u16::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
            WindDirection::new(value).map_err(|_| AprsError::InvalidFormat)
        })
        .transpose()?;
    let wind_speed = parse_unsigned_field(speed_bytes)?
        .map(|value| {
            let value = u16::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
            ThreeDigitWeatherValue::new(value).map_err(|_| AprsError::InvalidFormat)
        })
        .transpose()?;
    let tail = bytes.get(7..).ok_or(AprsError::InvalidFormat)?;
    let (mut weather, _) = parse_weather_fields(tail)?;
    weather.set_wind_direction(wind_direction);
    weather.set_wind_speed(wind_speed);
    Ok(Some(weather))
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
/// - `L`/`l` = luminosity below/above 1000 watts per square metre
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for an invalid prefix, missing
/// mandatory `c`/`s`/`g`/`t` tag, malformed fixed-width value, out-of-range
/// measurement, or duplicate/reordered tag.
pub fn parse_aprs_weather_positionless(
    info: &[u8],
) -> Result<AprsPositionlessWeatherReport, AprsError> {
    if info.first() != Some(&b'_') {
        return Err(AprsError::InvalidFormat);
    }
    let timestamp_bytes = info.get(1..9).ok_or(AprsError::InvalidFormat)?;
    let timestamp_wire = decode_wire_ascii("APRS weather timestamp", timestamp_bytes)?;
    let timestamp = AprsWeatherTimestamp::from_wire(timestamp_wire)?;
    let data = info.get(9..).ok_or(AprsError::InvalidFormat)?;
    for (offset, required_tag) in [(0, b'c'), (4, b's'), (8, b'g'), (12, b't')] {
        if data.get(offset) != Some(&required_tag) {
            return Err(AprsError::InvalidFormat);
        }
    }
    let (weather, consumed) = parse_weather_fields(data)?;
    let comment_bytes = data.get(consumed..).ok_or(AprsError::InvalidFormat)?;
    let comment = decode_wire_ascii("APRS weather comment", comment_bytes)?;
    let comment = WeatherComment::new(comment).map_err(|_| AprsError::InvalidFormat)?;
    Ok(AprsPositionlessWeatherReport::with_comment(
        timestamp, weather, comment,
    ))
}

/// Parse APRS weather data fields from a byte slice.
///
/// Weather fields are a contiguous sequence of `<tag><value>` pairs. APRS
/// permits later parameters in differing orders, but this crate deliberately
/// implements a strict canonical profile: wind direction, wind speed, gust,
/// temperature, rain 1h, rain 24h, rain since midnight, humidity, pressure,
/// then luminosity. The public positionless parser additionally requires the
/// first four tags, as APRS 1.0.1 §12 mandates; all dots or all spaces encode
/// a required field whose measurement is absent.
///
/// The parser walks the buffer from the start, consumes known tag/value pairs,
/// and advances. It stops on the first unknown tag, leaving the suffix as a
/// comment. A *known* tag is never reclassified as comment text: truncated or
/// malformed values are errors. Tags must be strictly increasing in the order
/// above, which also rejects duplicates.
///
/// This is strictly more correct than a `find()`-based scan, which would
/// false-match tag letters appearing inside comment text (e.g. `"canada"`
/// matching `c` for wind direction).
///
/// Private by design: callers outside this crate should use
/// [`parse_aprs_weather_positionless`] (which validates the leading
/// `_` + 8-byte timestamp and then delegates here) to avoid mistaking
/// non-weather bytes for weather data.
fn parse_weather_fields(data: &[u8]) -> Result<(AprsWeather, usize), AprsError> {
    let mut wx = AprsWeather::default();
    let mut i = 0;
    let mut last_order = None;
    while let Some(&tag) = data.get(i) {
        let (width, order) = match tag {
            b'c' => (3, 0),
            b's' => (3, 1),
            b'g' => (3, 2),
            b't' => (3, 3),
            b'r' => (3, 4),
            b'p' => (3, 5),
            b'P' => (3, 6),
            b'h' => (2, 7),
            b'b' => (5, 8),
            b'L' | b'l' => (3, 9),
            // Unknown byte: assume start of comment / type suffix.
            _ => break,
        };
        if last_order.is_some_and(|previous| order <= previous) {
            return Err(AprsError::InvalidFormat);
        }
        let val_bytes = data
            .get(i + 1..i + 1 + width)
            .ok_or(AprsError::InvalidFormat)?;
        match tag {
            b'c' => {
                let value = parse_unsigned_field(val_bytes)?
                    .map(|value| {
                        let value = u16::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
                        WindDirection::new(value).map_err(|_| AprsError::InvalidFormat)
                    })
                    .transpose()?;
                wx.set_wind_direction(value);
            }
            b's' => {
                wx.set_wind_speed(parse_three_digit_field(val_bytes)?);
            }
            b'g' => {
                wx.set_wind_gust(parse_three_digit_field(val_bytes)?);
            }
            b't' => {
                wx.set_temperature(parse_temperature_field(val_bytes)?);
            }
            b'r' => {
                wx.set_rain_1h(parse_three_digit_field(val_bytes)?);
            }
            b'p' => {
                wx.set_rain_24h(parse_three_digit_field(val_bytes)?);
            }
            b'P' => {
                wx.set_rain_since_midnight(parse_three_digit_field(val_bytes)?);
            }
            b'h' => {
                let value = parse_unsigned_field(val_bytes)?
                    .map(|value| {
                        let raw = u8::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
                        let percent = if raw == 0 { 100 } else { raw };
                        Humidity::new(percent).map_err(|_| AprsError::InvalidFormat)
                    })
                    .transpose()?;
                wx.set_humidity(value);
            }
            b'b' => {
                let value = parse_unsigned_field(val_bytes)?
                    .map(|value| {
                        BarometricPressure::new(value).map_err(|_| AprsError::InvalidFormat)
                    })
                    .transpose()?;
                wx.set_pressure(value);
            }
            b'L' | b'l' => {
                let value = parse_unsigned_field(val_bytes)?
                    .map(|value| {
                        let raw = u16::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
                        let watts = if tag == b'l' {
                            raw.checked_add(1_000).ok_or(AprsError::InvalidFormat)?
                        } else {
                            raw
                        };
                        Luminosity::new(watts).map_err(|_| AprsError::InvalidFormat)
                    })
                    .transpose()?;
                wx.set_luminosity(value);
            }
            _ => unreachable!("weather tag was classified above"),
        }
        last_order = Some(order);
        i += 1 + width;
    }
    Ok((wx, i))
}

/// Whether a field has a valid unsigned or no-data lexical shape.
fn is_unsigned_or_placeholder(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_digit)
        || bytes.iter().all(|byte| *byte == b'.')
        || bytes.iter().all(|byte| *byte == b' ')
}

/// Parse an exact-width unsigned decimal field or an all-dot/all-space
/// no-data placeholder.
fn parse_unsigned_field(bytes: &[u8]) -> Result<Option<u32>, AprsError> {
    if bytes.iter().all(|byte| *byte == b'.') || bytes.iter().all(|byte| *byte == b' ') {
        return Ok(None);
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return Err(AprsError::InvalidFormat);
    }
    let mut value = 0u32;
    for byte in bytes {
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(u32::from(*byte - b'0')))
            .ok_or(AprsError::InvalidFormat)?;
    }
    Ok(Some(value))
}

fn parse_three_digit_field(bytes: &[u8]) -> Result<Option<ThreeDigitWeatherValue>, AprsError> {
    parse_unsigned_field(bytes)?
        .map(|value| {
            let value = u16::try_from(value).map_err(|_| AprsError::InvalidFormat)?;
            ThreeDigitWeatherValue::new(value).map_err(|_| AprsError::InvalidFormat)
        })
        .transpose()
}

fn parse_temperature_field(bytes: &[u8]) -> Result<Option<Fahrenheit>, AprsError> {
    if bytes.iter().all(|byte| *byte == b'.') || bytes.iter().all(|byte| *byte == b' ') {
        return Ok(None);
    }
    if bytes.len() != 3 {
        return Err(AprsError::InvalidFormat);
    }
    let value = if bytes.iter().all(u8::is_ascii_digit) {
        let raw = parse_unsigned_field(bytes)?.ok_or(AprsError::InvalidFormat)?;
        i16::try_from(raw).map_err(|_| AprsError::InvalidFormat)?
    } else if bytes.first() == Some(&b'-')
        && bytes
            .get(1..)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
    {
        let magnitude = parse_unsigned_field(bytes.get(1..).ok_or(AprsError::InvalidFormat)?)?
            .ok_or(AprsError::InvalidFormat)?;
        if magnitude == 0 {
            return Err(AprsError::InvalidFormat);
        }
        -i16::try_from(magnitude).map_err(|_| AprsError::InvalidFormat)?
    } else {
        return Err(AprsError::InvalidFormat);
    };
    Fahrenheit::new(value)
        .map(Some)
        .map_err(|_| AprsError::InvalidFormat)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn typed_weather_values_enforce_wire_ranges() -> TestResult {
        assert_eq!(WindDirection::new(360)?.degrees(), 360);
        assert!(matches!(
            WindDirection::new(361),
            Err(WeatherValueError::WindDirectionOutOfRange { value: 361 })
        ));

        assert_eq!(ThreeDigitWeatherValue::new(999)?.value(), 999);
        assert!(matches!(
            ThreeDigitWeatherValue::new(1_000),
            Err(WeatherValueError::ThreeDigitValueOutOfRange { value: 1_000 })
        ));

        assert_eq!(Humidity::new(1)?.percent(), 1);
        assert_eq!(Humidity::new(100)?.percent(), 100);
        assert!(Humidity::new(0).is_err());
        assert!(Humidity::new(101).is_err());

        assert_eq!(BarometricPressure::new(99_999)?.tenths_hpa(), 99_999);
        assert!(BarometricPressure::new(100_000).is_err());
        assert_eq!(Luminosity::new(1_999)?.watts_per_square_meter(), 1_999);
        assert!(Luminosity::new(2_000).is_err());

        assert_eq!(WindDirection::try_from(180)?.to_string(), "180");
        assert_eq!(u16::from(ThreeDigitWeatherValue::new(42)?), 42);
        Ok(())
    }

    #[test]
    fn empty_weather_default_is_valid_and_absent() {
        let weather = AprsWeather::default();
        assert_eq!(weather, AprsWeather::new());
        assert_eq!(weather.wind_direction(), None);
        assert_eq!(weather.wind_speed(), None);
        assert_eq!(weather.wind_gust(), None);
        assert_eq!(weather.temperature(), None);
        assert_eq!(weather.rain_1h(), None);
        assert_eq!(weather.rain_24h(), None);
        assert_eq!(weather.rain_since_midnight(), None);
        assert_eq!(weather.humidity(), None);
        assert_eq!(weather.pressure(), None);
        assert_eq!(weather.luminosity(), None);
    }

    #[test]
    fn parse_weather_positionless_full_preserves_every_field() -> TestResult {
        let report = parse_aprs_weather_positionless(
            b"_01011234c180s005g010t075r001p010P020h55b10135L875Dvs",
        )?;
        assert_eq!(report.timestamp.to_wire_string(), "01011234");
        assert_eq!(
            report.weather.wind_direction().map(WindDirection::degrees),
            Some(180)
        );
        assert_eq!(
            report
                .weather
                .wind_speed()
                .map(ThreeDigitWeatherValue::value),
            Some(5)
        );
        assert_eq!(
            report
                .weather
                .wind_gust()
                .map(ThreeDigitWeatherValue::value),
            Some(10)
        );
        assert_eq!(report.weather.temperature().map(Fahrenheit::get), Some(75));
        assert_eq!(
            report.weather.rain_1h().map(ThreeDigitWeatherValue::value),
            Some(1)
        );
        assert_eq!(
            report.weather.rain_24h().map(ThreeDigitWeatherValue::value),
            Some(10)
        );
        assert_eq!(
            report
                .weather
                .rain_since_midnight()
                .map(ThreeDigitWeatherValue::value),
            Some(20)
        );
        assert_eq!(report.weather.humidity().map(Humidity::percent), Some(55));
        assert_eq!(
            report
                .weather
                .pressure()
                .map(BarometricPressure::tenths_hpa),
            Some(10_135)
        );
        assert_eq!(
            report
                .weather
                .luminosity()
                .map(Luminosity::watts_per_square_meter),
            Some(875)
        );
        assert_eq!(report.comment(), "Dvs");
        Ok(())
    }

    #[test]
    fn parse_weather_lowercase_luminosity_adds_thousands_offset() -> TestResult {
        let report = parse_aprs_weather_positionless(b"_01011234c...s...g...t...l234")?;
        assert_eq!(
            report
                .weather
                .luminosity()
                .map(Luminosity::watts_per_square_meter),
            Some(1_234)
        );
        Ok(())
    }

    #[test]
    fn parse_weather_missing_fields_and_humidity_100() -> TestResult {
        let report = parse_aprs_weather_positionless(b"_01011234c...s   g...t072h00")?;
        assert_eq!(report.weather.wind_direction(), None);
        assert_eq!(report.weather.wind_speed(), None);
        assert_eq!(report.weather.temperature().map(Fahrenheit::get), Some(72));
        assert_eq!(report.weather.humidity().map(Humidity::percent), Some(100));
        Ok(())
    }

    #[test]
    fn parse_weather_stops_only_on_unknown_comment_tag() -> TestResult {
        let report = parse_aprs_weather_positionless(b"_01011234c...s...g...t072Jim")?;
        assert_eq!(report.weather.temperature().map(Fahrenheit::get), Some(72));
        assert_eq!(report.comment(), "Jim");
        Ok(())
    }

    #[test]
    fn parse_weather_fields_allow_ordered_gaps() -> TestResult {
        let (weather, consumed) = parse_weather_fields(b"t072h50")?;
        assert_eq!(weather.temperature().map(Fahrenheit::get), Some(72));
        assert_eq!(weather.wind_direction(), None);
        assert_eq!(weather.humidity().map(Humidity::percent), Some(50));
        assert_eq!(consumed, 7);
        Ok(())
    }

    #[test]
    fn parse_weather_rejects_malformed_known_tags() {
        for info in [
            b"_01011234c-12s...g...t...".as_slice(),
            b"_01011234c12".as_slice(),
            b"_01011234c. .s...g...t...".as_slice(),
            b"_01011234c...s12".as_slice(),
            b"_01011234c...s1x2g...t...".as_slice(),
            b"_01011234c...s...g12".as_slice(),
            b"_01011234c...s...g1x2t...".as_slice(),
            b"_01011234c...s...g...t12".as_slice(),
            b"_01011234c...s...g...t+72".as_slice(),
            b"_01011234c...s...g...t-00".as_slice(),
            b"_01011234c...s...g...t...r12".as_slice(),
            b"_01011234c...s...g...t...p12".as_slice(),
            b"_01011234c...s...g...t...P12".as_slice(),
            b"_01011234c...s...g...t...h1".as_slice(),
            b"_01011234c...s...g...t...b1234".as_slice(),
            b"_01011234c...s...g...t...L12".as_slice(),
            b"_01011234c...s...g...t...l12".as_slice(),
            b"_01011234c...s...g...t...r1x2".as_slice(),
            b"_01011234c...s...g...t...p1x2".as_slice(),
            b"_01011234c...s...g...t...P1x2".as_slice(),
            b"_01011234c...s...g...t...h-1".as_slice(),
            b"_01011234c...s...g...t...b10x35".as_slice(),
            b"_01011234c...s...g...t...L8x5".as_slice(),
            b"_01011234c...s...g...t...l8x5".as_slice(),
            b"_01011234c...s...g...t072canada".as_slice(),
        ] {
            assert!(
                matches!(
                    parse_aprs_weather_positionless(info),
                    Err(AprsError::InvalidFormat)
                ),
                "malformed known field accepted: {info:?}",
            );
        }
    }

    #[test]
    fn parse_weather_rejects_duplicate_and_reordered_tags() {
        for info in [
            b"_01011234c180s...g...t...c090".as_slice(),
            b"_01011234c...s...g...t...L500l200".as_slice(),
            b"_01011234c...s...g...t...g005".as_slice(),
            b"_01011234c...s...g...t...b10135h55".as_slice(),
        ] {
            assert!(
                matches!(
                    parse_aprs_weather_positionless(info),
                    Err(AprsError::InvalidFormat)
                ),
                "duplicate/reordered weather field accepted: {info:?}",
            );
        }
    }

    #[test]
    fn positionless_weather_requires_mandatory_core_tags() {
        for info in [
            b"_01011234".as_slice(),
            b"_01011234c...".as_slice(),
            b"_01011234c...s...".as_slice(),
            b"_01011234c...s...g...".as_slice(),
            b"_01011234c...s...t...".as_slice(),
        ] {
            assert!(
                matches!(
                    parse_aprs_weather_positionless(info),
                    Err(AprsError::InvalidFormat)
                ),
                "positionless report missing required core tag was accepted: {info:?}",
            );
        }
    }

    #[test]
    fn extract_position_weather_preserves_typed_values() -> TestResult {
        let dotted = extract_position_weather('_', ".../...g005t072h50b10132")?
            .ok_or("dotted wind must still parse as weather")?;
        assert_eq!(dotted.wind_direction(), None);
        assert_eq!(dotted.wind_speed(), None);
        assert_eq!(
            dotted.wind_gust().map(ThreeDigitWeatherValue::value),
            Some(5)
        );
        assert_eq!(dotted.temperature().map(Fahrenheit::get), Some(72));

        let populated = extract_position_weather('_', "220/004g005t077r000p000P000h50b09900L999")?
            .ok_or("digit wind must parse")?;
        assert_eq!(
            populated.wind_direction().map(WindDirection::degrees),
            Some(220)
        );
        assert_eq!(
            populated.wind_speed().map(ThreeDigitWeatherValue::value),
            Some(4)
        );
        assert_eq!(
            populated.pressure().map(BarometricPressure::tenths_hpa),
            Some(9_900)
        );
        assert_eq!(
            populated
                .luminosity()
                .map(Luminosity::watts_per_square_meter),
            Some(999)
        );
        Ok(())
    }

    #[test]
    fn extract_position_weather_distinguishes_non_extension_and_invalid_weather() -> TestResult {
        assert_eq!(extract_position_weather('_', "abc/defghi")?, None);
        assert_eq!(extract_position_weather('>', "220/004g005")?, None);
        assert!(extract_position_weather('_', "361/004g005").is_err());
        assert!(extract_position_weather('_', "220/004t077g005").is_err());
        Ok(())
    }

    #[test]
    fn parse_weather_fully_dotted_spec_example() -> TestResult {
        let report = parse_aprs_weather_positionless(b"_10090556c...s...g...t...P012Jim")?;
        assert_eq!(report.weather.wind_direction(), None);
        assert_eq!(report.weather.wind_speed(), None);
        assert_eq!(report.weather.wind_gust(), None);
        assert_eq!(report.weather.temperature(), None);
        assert_eq!(
            report
                .weather
                .rain_since_midnight()
                .map(ThreeDigitWeatherValue::value),
            Some(12)
        );
        assert_eq!(report.comment(), "Jim");
        Ok(())
    }

    #[test]
    fn positionless_weather_rejects_missing_or_invalid_timestamp() {
        for info in [
            b"_".as_slice(),
            b"_0101123".as_slice(),
            b"_00011234t072".as_slice(),
            b"_02301234t072".as_slice(),
            b"_01012400t072".as_slice(),
            b"_01011260t072".as_slice(),
            b"_01AA1234t072".as_slice(),
        ] {
            assert!(
                parse_aprs_weather_positionless(info).is_err(),
                "invalid weather timestamp accepted: {info:?}"
            );
        }
    }
}
