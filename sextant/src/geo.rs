// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Geographic position type and GPS-sentence parsing.
//!
//! D-STAR slow-data GPS arrives as either a DPRS `$$CRC...` sentence
//! (decoded via `dstar_gateway_core::dprs`) or a raw NMEA `$GPRMC` /
//! `$GPGGA` sentence (decoded here). Both resolve to a [`GpsPosition`].

use dstar_gateway_core::dprs::parse_dprs;

/// A decoded geographic position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GpsPosition {
    /// Latitude in decimal degrees, positive North.
    pub(crate) latitude: f64,
    /// Longitude in decimal degrees, positive East.
    pub(crate) longitude: f64,
}

impl GpsPosition {
    /// Construct, rejecting out-of-range or non-finite coordinates.
    pub(crate) fn new(latitude: f64, longitude: f64) -> Option<Self> {
        if !latitude.is_finite() || !longitude.is_finite() {
            return None;
        }
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return None;
        }
        Some(Self {
            latitude,
            longitude,
        })
    }
}

/// Try to decode a GPS sentence (DPRS or NMEA) into a position.
///
/// Returns `None` for anything that does not parse — callers treat a
/// `None` as "no position yet", never an error (lenient receive).
pub(crate) fn parse_gps_sentence(sentence: &str) -> Option<GpsPosition> {
    let trimmed = sentence.trim();
    if trimmed.starts_with("$$CRC") {
        let report = parse_dprs(trimmed).ok()?;
        return GpsPosition::new(report.latitude.degrees(), report.longitude.degrees());
    }
    if trimmed.starts_with("$GPRMC") || trimmed.starts_with("$GNRMC") {
        return parse_rmc(trimmed);
    }
    if trimmed.starts_with("$GPGGA") || trimmed.starts_with("$GNGGA") {
        return parse_gga(trimmed);
    }
    None
}

/// Parse the lat/lon out of an NMEA `RMC` sentence.
/// Field layout: `$GPRMC,time,status,lat,N/S,lon,E/W,...`
fn parse_rmc(sentence: &str) -> Option<GpsPosition> {
    let f: Vec<&str> = sentence.split(',').collect();
    let lat = nmea_coord(f.get(3)?, f.get(4)?)?;
    let lon = nmea_coord(f.get(5)?, f.get(6)?)?;
    GpsPosition::new(lat, lon)
}

/// Parse the lat/lon out of an NMEA `GGA` sentence.
/// Field layout: `$GPGGA,time,lat,N/S,lon,E/W,...`
fn parse_gga(sentence: &str) -> Option<GpsPosition> {
    let f: Vec<&str> = sentence.split(',').collect();
    let lat = nmea_coord(f.get(2)?, f.get(3)?)?;
    let lon = nmea_coord(f.get(4)?, f.get(5)?)?;
    GpsPosition::new(lat, lon)
}

/// Convert an NMEA `DDDMM.MMMM` + hemisphere pair to decimal degrees.
fn nmea_coord(value: &str, hemisphere: &str) -> Option<f64> {
    if value.is_empty() {
        return None;
    }
    let dot = value.find('.')?;
    // Degrees are all digits before the last two ahead of the dot.
    let deg_end = dot.checked_sub(2)?;
    let degrees: f64 = value.get(..deg_end)?.parse().ok()?;
    let minutes: f64 = value.get(deg_end..)?.parse().ok()?;
    let mut decimal = degrees + minutes / 60.0;
    if matches!(hemisphere, "S" | "W") {
        decimal = -decimal;
    }
    Some(decimal)
}

/// An operator-entered position to transmit in slow data.
///
/// Distinct from [`GpsPosition`] (a decoded *received* position):
/// this carries the APRS symbol glyph and a free-text comment the
/// operator picks for their own beacon.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TxPosition {
    /// Latitude in decimal degrees, positive North.
    pub(crate) latitude: f64,
    /// Longitude in decimal degrees, positive East.
    pub(crate) longitude: f64,
    /// APRS symbol glyph (e.g. `/` car, `-` house).
    pub(crate) symbol: char,
    /// Free-text comment appended to the DPRS sentence.
    pub(crate) comment: String,
}

impl TxPosition {
    /// Validate the coordinates. Returns `None` for non-finite or
    /// out-of-range latitude/longitude.
    pub(crate) fn validated(&self) -> Option<&Self> {
        GpsPosition::new(self.latitude, self.longitude).map(|_| self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (pos.latitude - 48.1173).abs() < 0.001,
            "lat {}",
            pos.latitude
        );
        assert!(
            (pos.longitude - 11.5167).abs() < 0.001,
            "lon {}",
            pos.longitude
        );
        Ok(())
    }

    #[test]
    fn parse_gga_decodes_position() -> TestResult {
        let s = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,";
        let pos = parse_gps_sentence(s).ok_or("GGA did not parse")?;
        assert!((pos.latitude - 48.1173).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn parse_southern_western_negates() -> TestResult {
        let s = "$GPRMC,123519,A,3349.000,S,15112.000,W,0,0,230394,0,E";
        let pos = parse_gps_sentence(s).ok_or("RMC did not parse")?;
        assert!(pos.latitude < 0.0, "S latitude must be negative");
        assert!(pos.longitude < 0.0, "W longitude must be negative");
        Ok(())
    }

    #[test]
    fn non_gps_sentence_returns_none() {
        assert!(parse_gps_sentence("hello world").is_none());
        assert!(parse_gps_sentence("").is_none());
    }

    #[test]
    fn tx_position_validates_range() {
        let good = TxPosition {
            latitude: 35.5,
            longitude: -82.55,
            symbol: '/',
            comment: "test".into(),
        };
        assert!(good.validated().is_some());
        let bad = TxPosition {
            latitude: 999.0,
            longitude: 0.0,
            symbol: '/',
            comment: String::new(),
        };
        assert!(bad.validated().is_none());
    }
}
