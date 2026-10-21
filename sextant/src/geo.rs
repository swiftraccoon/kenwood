// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Geographic position type and GPS-sentence parsing.
//!
//! D-STAR slow-data GPS arrives as either a DPRS `$$CRC...` sentence
//! (decoded via `dstar_gateway_core::dprs`) or a raw NMEA `$GPRMC` /
//! `$GPGGA` sentence (decoded here). Both resolve to a [`GpsPosition`].

use dstar_gateway_core::dprs::{
    DprsComment, DprsError, DprsPosition, DprsReport, DprsSymbol, Latitude, Longitude, SymbolCode,
    SymbolTable,
};

/// Application limit for one assembled GPS sentence, including its terminator.
pub(crate) const MAX_GPS_SENTENCE_BYTES: usize = 256;

/// A decoded geographic position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GpsPosition {
    /// Latitude in decimal degrees, positive North.
    latitude: Latitude,
    /// Longitude in decimal degrees, positive East.
    longitude: Longitude,
}

impl GpsPosition {
    /// Construct, rejecting out-of-range or non-finite coordinates.
    pub(crate) fn new(latitude: f64, longitude: f64) -> Option<Self> {
        Some(Self {
            latitude: Latitude::new(latitude).ok()?,
            longitude: Longitude::new(longitude).ok()?,
        })
    }

    /// Signed decimal degrees north of the equator.
    pub(crate) const fn latitude(self) -> f64 {
        self.latitude.degrees()
    }

    /// Signed decimal degrees east of Greenwich.
    pub(crate) const fn longitude(self) -> f64 {
        self.longitude.degrees()
    }

    /// Checked coordinates used by DPRS report encoding.
    const fn coordinates(self) -> DprsPosition {
        DprsPosition {
            latitude: self.latitude,
            longitude: self.longitude,
        }
    }
}

/// Try to decode a GPS sentence (DPRS or NMEA) into a position.
///
/// DPRS requires a matching checksum. NMEA accepts checksumless receive text;
/// an included checksum must match, RMC must report active status, and GGA must
/// report a positive fix quality. Malformed positions return `None`.
pub(crate) fn parse_gps_sentence(sentence: &str) -> Option<GpsPosition> {
    if sentence.len() > MAX_GPS_SENTENCE_BYTES {
        return None;
    }
    let trimmed = sentence.trim_end_matches(['\r', '\n']);
    if trimmed.starts_with("$$CRC") {
        let report: DprsReport = trimmed.parse().ok()?;
        return GpsPosition::new(report.latitude().degrees(), report.longitude().degrees());
    }
    let body = checked_nmea_body(trimmed)?;
    if body.starts_with("$GPRMC,") || body.starts_with("$GNRMC,") {
        return parse_rmc(body);
    }
    if body.starts_with("$GPGGA,") || body.starts_with("$GNGGA,") {
        return parse_gga(body);
    }
    None
}

/// Parse the lat/lon out of an NMEA `RMC` sentence.
/// Field layout: `$GPRMC,time,status,lat,N/S,lon,E/W,...`
fn parse_rmc(sentence: &str) -> Option<GpsPosition> {
    let f: Vec<&str> = sentence.split(',').collect();
    if f.get(2) != Some(&"A") {
        return None;
    }
    let lat = nmea_coord(f.get(3)?, f.get(4)?, CoordinateAxis::Latitude)?;
    let lon = nmea_coord(f.get(5)?, f.get(6)?, CoordinateAxis::Longitude)?;
    GpsPosition::new(lat, lon)
}

/// Parse the lat/lon out of an NMEA `GGA` sentence.
/// Field layout: `$GPGGA,time,lat,N/S,lon,E/W,...`
fn parse_gga(sentence: &str) -> Option<GpsPosition> {
    let f: Vec<&str> = sentence.split(',').collect();
    if f.get(6)?.parse::<u8>().ok()? == 0 {
        return None;
    }
    let lat = nmea_coord(f.get(2)?, f.get(3)?, CoordinateAxis::Latitude)?;
    let lon = nmea_coord(f.get(4)?, f.get(5)?, CoordinateAxis::Longitude)?;
    GpsPosition::new(lat, lon)
}

#[derive(Clone, Copy)]
enum CoordinateAxis {
    Latitude,
    Longitude,
}

/// Convert the axis-specific degree/minute spelling and hemisphere.
fn nmea_coord(value: &str, hemisphere: &str, axis: CoordinateAxis) -> Option<f64> {
    let (width, negative) = match (axis, hemisphere) {
        (CoordinateAxis::Latitude, "N") => (2, false),
        (CoordinateAxis::Latitude, "S") => (2, true),
        (CoordinateAxis::Longitude, "E") => (3, false),
        (CoordinateAxis::Longitude, "W") => (3, true),
        _ => return None,
    };
    let (whole, fraction) = value.split_once('.')?;
    if whole.len() != width + 2
        || fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let degrees: f64 = whole.get(..width)?.parse().ok()?;
    let minutes: f64 = value.get(width..)?.parse().ok()?;
    if !(0.0..60.0).contains(&minutes) {
        return None;
    }
    let decimal = degrees + minutes / 60.0;
    Some(if negative { -decimal } else { decimal })
}

/// Verify an optional two-hex-digit NMEA XOR checksum before field parsing.
fn checked_nmea_body(sentence: &str) -> Option<&str> {
    if !sentence.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        return None;
    }
    let payload = sentence.strip_prefix('$')?;
    let Some((body, checksum)) = payload.split_once('*') else {
        return Some(sentence);
    };
    if checksum.len() != 2 {
        return None;
    }
    let expected = u8::from_str_radix(checksum, 16).ok()?;
    let actual = body.bytes().fold(0, |checksum, byte| checksum ^ byte);
    if expected != actual {
        return None;
    }
    sentence.get(..body.len() + 1)
}

/// An operator-entered position to transmit in slow data.
///
/// Distinct from [`GpsPosition`] (a decoded *received* position):
/// this carries the APRS symbol glyph and a free-text comment the
/// operator picks for their own beacon.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TxPosition {
    position: GpsPosition,
    symbol: DprsSymbol,
    comment: DprsComment,
}

impl TxPosition {
    /// Validate coordinates, primary-table symbol and the plain-text DPRS comment.
    pub(crate) fn new(
        latitude: f64,
        longitude: f64,
        symbol: char,
        comment: &str,
    ) -> Result<Self, DprsError> {
        let coordinates = DprsPosition {
            latitude: Latitude::new(latitude)?,
            longitude: Longitude::new(longitude)?,
        };
        Ok(Self {
            position: GpsPosition {
                latitude: coordinates.latitude,
                longitude: coordinates.longitude,
            },
            symbol: DprsSymbol::new(SymbolTable::Primary, SymbolCode::new(symbol)?),
            comment: DprsComment::new(comment)?,
        })
    }

    /// Checked geographic position used for distance and bearing.
    pub(crate) const fn position(&self) -> GpsPosition {
        self.position
    }

    /// Checked report body, with the caller-selected source identifier.
    pub(crate) fn report(&self, source: dstar_gateway_core::dprs::DprsSource) -> DprsReport {
        DprsReport::new(
            source,
            self.position.coordinates(),
            self.symbol,
            self.comment.clone(),
        )
    }
}

/// Mean Earth radius in kilometres (IUGG).
const EARTH_RADIUS_KM: f64 = 6_371.008_8;

/// Great-circle distance in kilometres between two positions
/// (haversine formula).
pub(crate) fn haversine_km(from: GpsPosition, to: GpsPosition) -> f64 {
    let (lat1, lon1) = (from.latitude().to_radians(), from.longitude().to_radians());
    let (lat2, lon2) = (to.latitude().to_radians(), to.longitude().to_radians());
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a =
        (lat1.cos() * lat2.cos()).mul_add((dlon / 2.0).sin().powi(2), (dlat / 2.0).sin().powi(2));
    2.0 * EARTH_RADIUS_KM * a.clamp(0.0, 1.0).sqrt().asin()
}

/// Initial great-circle bearing in degrees (0° = north, clockwise)
/// from `from` towards `to`.
pub(crate) fn initial_bearing_deg(from: GpsPosition, to: GpsPosition) -> f64 {
    let (lat1, lon1) = (from.latitude().to_radians(), from.longitude().to_radians());
    let (lat2, lon2) = (to.latitude().to_radians(), to.longitude().to_radians());
    let dlon = lon2 - lon1;
    let y = dlon.sin() * lat2.cos();
    let x = lat1
        .cos()
        .mul_add(lat2.sin(), -(lat1.sin() * lat2.cos() * dlon.cos()));
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JFK → LHR, the canonical great-circle test pair
    /// (≈5 555 km, initial bearing ≈51°).
    #[test]
    fn haversine_and_bearing_match_known_pair() -> TestResult {
        let jfk = GpsPosition::new(40.6413, -73.7781).ok_or("JFK")?;
        let lhr = GpsPosition::new(51.4700, -0.4543).ok_or("LHR")?;
        let dist = haversine_km(jfk, lhr);
        assert!(
            (dist - 5_555.0).abs() < 30.0,
            "JFK-LHR ≈ 5555 km, got {dist:.1}"
        );
        let bearing = initial_bearing_deg(jfk, lhr);
        assert!(
            (bearing - 51.0).abs() < 2.0,
            "JFK→LHR initial bearing ≈ 51°, got {bearing:.1}"
        );
        Ok(())
    }

    #[test]
    fn zero_distance_for_identical_points() -> TestResult {
        let p = GpsPosition::new(41.7148, -72.7273).ok_or("position")?;
        assert!(haversine_km(p, p) < 1e-9);
        Ok(())
    }

    #[test]
    fn bearing_is_normalized_to_compass_range() -> TestResult {
        // Due west should be 270°, not -90°.
        let bearing = initial_bearing_deg(
            GpsPosition::new(0.0, 0.0).ok_or("origin")?,
            GpsPosition::new(0.0, -10.0).ok_or("destination")?,
        );
        assert!(
            (bearing - 270.0).abs() < 1e-6,
            "due west = 270°, got {bearing}"
        );
        Ok(())
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn gps_position_rejects_out_of_range() {
        assert!(GpsPosition::new(91.0, 0.0).is_none());
        assert!(GpsPosition::new(0.0, 181.0).is_none());
        assert!(GpsPosition::new(f64::NAN, 0.0).is_none());
        assert!(GpsPosition::new(45.0, -120.0).is_some());
    }

    #[test]
    fn parse_rmc_decodes_position() -> TestResult {
        // 4807.038,N = 48°07.038' N ; 01131.000,E = 11°31.000' E
        let s = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W";
        let pos = parse_gps_sentence(s).ok_or("RMC did not parse")?;
        assert!(
            (pos.latitude() - 48.1173).abs() < 0.001,
            "lat {}",
            pos.latitude()
        );
        assert!(
            (pos.longitude() - 11.5167).abs() < 0.001,
            "lon {}",
            pos.longitude()
        );
        Ok(())
    }

    #[test]
    fn parse_gga_decodes_position() -> TestResult {
        let s = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,";
        let pos = parse_gps_sentence(s).ok_or("GGA did not parse")?;
        assert!((pos.latitude() - 48.1173).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn parse_southern_western_negates() -> TestResult {
        let s = "$GPRMC,123519,A,3349.000,S,15112.000,W,0,0,230394,0,E";
        let pos = parse_gps_sentence(s).ok_or("RMC did not parse")?;
        assert!(pos.latitude() < 0.0, "S latitude must be negative");
        assert!(pos.longitude() < 0.0, "W longitude must be negative");
        Ok(())
    }

    #[test]
    fn non_gps_sentence_returns_none() {
        assert!(parse_gps_sentence("hello world").is_none());
        assert!(parse_gps_sentence("").is_none());
    }

    #[test]
    fn tx_position_validates_range() {
        assert!(TxPosition::new(35.5, -82.55, '/', "test").is_ok());
        assert!(TxPosition::new(999.0, 0.0, '/', "").is_err());
        assert!(TxPosition::new(0.0, 0.0, ' ', "").is_err());
        assert!(TxPosition::new(0.0, 0.0, '/', "bad\ncomment").is_err());
        assert!(TxPosition::new(0.0, 0.0, '/', &"x".repeat(44)).is_err());
    }

    #[test]
    fn nmea_rejects_invalid_fix_hemisphere_minutes_and_checksum() {
        let valid = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
        assert!(parse_gps_sentence(valid).is_some());
        for invalid in [
            "$GPRMC,123519,V,4807.038,N,01131.000,E,0,0",
            "$GPRMC,123519,A,4860.000,N,01131.000,E,0,0",
            "$GPRMC,123519,A,4807.038,E,01131.000,E,0,0",
            "$GPRMC,123519,A,4807.038,N,01131.000,Q,0,0",
            "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*00",
            "$GPGGA,123519,4807.038,N,01131.000,E,0,08,0.9,545.4,M,46.9,M,,",
        ] {
            assert!(parse_gps_sentence(invalid).is_none(), "{invalid}");
        }
    }
}
