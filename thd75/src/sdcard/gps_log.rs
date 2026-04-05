//! Parser for NMEA 0183 GPS log `.nme` files.
//!
//! The TH-D75 records GPS track logs in standard NMEA 0183 format.
//! Each file contains a sequence of NMEA sentences, primarily `$GPRMC`
//! (recommended minimum) and `$GPGGA` (fix data) with time, position,
//! speed, course, and altitude.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/GPS_LOG/*.nme` — maximum 255 files per directory.
//!
//! # GPS Receiver mode (per Operating Tips §5.14.2)
//!
//! For prolonged GPS track logging, Menu No. 403 enables GPS Receiver
//! mode, which disables the transceiver function to conserve battery.
//! The FM broadcast radio remains functional in this mode.
//!
//! # Format
//!
//! Plain ASCII text, one NMEA sentence per line, terminated by `\r\n`.
//! Each sentence starts with `$` and ends with `*HH` where HH is a
//! two-digit hex XOR checksum of the bytes between `$` and `*`.
//!
//! # Supported Sentences
//!
//! | Sentence | Description |
//! |----------|-------------|
//! | `$GPRMC` | Recommended minimum: time, status, lat, lon, speed, course, date |
//! | `$GPGGA` | Fix data: time, lat, lon, quality, satellites, HDOP, altitude |

use super::SdCardError;

/// A parsed GPS position from an NMEA sentence.
///
/// `None` when the GPS has no fix (void RMC or quality=0 GGA).
pub type GpsPosition = Option<LatLon>;

/// Latitude/longitude in decimal degrees.
#[derive(Debug, Clone, PartialEq)]
pub struct LatLon {
    /// Latitude in decimal degrees (positive = N, negative = S).
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = E, negative = W).
    pub longitude: f64,
}

/// A single parsed NMEA RMC (Recommended Minimum) fix.
///
/// Contains the essential navigation data: time, position, speed,
/// course, and date. This is the primary sentence type in TH-D75 GPS logs.
#[derive(Debug, Clone, PartialEq)]
pub struct RmcFix {
    /// UTC time as `HHMMSS.sss` string (e.g., `"143025.000"`).
    pub utc_time: String,
    /// Fix status: `true` = valid (`A`), `false` = void (`V`).
    pub valid: bool,
    /// Position (latitude, longitude in decimal degrees).
    pub position: GpsPosition,
    /// Speed over ground in knots.
    pub speed_knots: f64,
    /// Course over ground in degrees true.
    pub course_degrees: f64,
    /// UTC date as `DDMMYY` string (e.g., `"030426"`).
    pub date: String,
}

/// A single parsed NMEA GGA (Global Positioning System Fix Data) fix.
///
/// Adds altitude and satellite information not present in RMC.
#[derive(Debug, Clone, PartialEq)]
pub struct GgaFix {
    /// UTC time as `HHMMSS.sss` string.
    pub utc_time: String,
    /// Position (latitude, longitude in decimal degrees).
    pub position: GpsPosition,
    /// GPS quality indicator (0=invalid, 1=GPS fix, 2=DGPS, etc.).
    pub quality: u8,
    /// Number of satellites in use.
    pub satellites: u8,
    /// Horizontal dilution of precision.
    pub hdop: f64,
    /// Altitude above mean sea level in metres.
    pub altitude_m: f64,
}

/// A parsed NMEA sentence.
#[derive(Debug, Clone, PartialEq)]
pub enum NmeaSentence {
    /// `$GPRMC` — Recommended Minimum (time, position, speed, course, date).
    Rmc(RmcFix),
    /// `$GPGGA` — Fix data (time, position, quality, satellites, altitude).
    Gga(GgaFix),
}

/// A complete parsed GPS log file.
#[derive(Debug, Clone)]
pub struct GpsLog {
    /// All successfully parsed sentences in file order.
    pub sentences: Vec<NmeaSentence>,
    /// Number of lines that failed to parse.
    pub errors: usize,
}

impl GpsLog {
    /// Return only RMC fixes, in file order.
    #[must_use]
    pub fn rmc_fixes(&self) -> Vec<&RmcFix> {
        self.sentences
            .iter()
            .filter_map(|s| match s {
                NmeaSentence::Rmc(fix) => Some(fix),
                NmeaSentence::Gga(_) => None,
            })
            .collect()
    }

    /// Return only GGA fixes, in file order.
    #[must_use]
    pub fn gga_fixes(&self) -> Vec<&GgaFix> {
        self.sentences
            .iter()
            .filter_map(|s| match s {
                NmeaSentence::Gga(fix) => Some(fix),
                NmeaSentence::Rmc(_) => None,
            })
            .collect()
    }

    /// Return only valid RMC fixes (status = 'A').
    #[must_use]
    pub fn valid_fixes(&self) -> Vec<&RmcFix> {
        self.rmc_fixes().into_iter().filter(|f| f.valid).collect()
    }
}

/// Parse an NMEA GPS log file from raw bytes.
///
/// Parses all `$GPRMC` and `$GPGGA` sentences. Unrecognised sentence
/// types and malformed lines are silently skipped (counted in
/// [`GpsLog::errors`]).
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] only if the input is completely
/// empty. Individual malformed sentences are skipped, not fatal.
pub fn parse(data: &[u8]) -> Result<GpsLog, SdCardError> {
    if data.is_empty() {
        return Err(SdCardError::FileTooSmall {
            expected: 1,
            actual: 0,
        });
    }

    let text = std::str::from_utf8(data).unwrap_or("");

    // If UTF-8 failed, try as Latin-1 (every byte is valid)
    let owned;
    let text = if text.is_empty() && !data.is_empty() {
        owned = data.iter().map(|&b| b as char).collect::<String>();
        &owned
    } else {
        text
    };

    let mut sentences = Vec::new();
    let mut errors = 0;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('$') {
            continue;
        }

        // Validate checksum
        if !verify_checksum(line) {
            errors += 1;
            continue;
        }

        // Strip checksum suffix for parsing
        let payload = line.find('*').map_or(line, |star| &line[..star]);

        let fields: Vec<&str> = payload.split(',').collect();

        match fields.first().copied() {
            Some("$GPRMC" | "$GNRMC") => {
                if let Some(fix) = parse_rmc(&fields) {
                    sentences.push(NmeaSentence::Rmc(fix));
                } else {
                    errors += 1;
                }
            }
            Some("$GPGGA" | "$GNGGA") => {
                if let Some(fix) = parse_gga(&fields) {
                    sentences.push(NmeaSentence::Gga(fix));
                } else {
                    errors += 1;
                }
            }
            _ => {
                // Unrecognised sentence type — skip silently
            }
        }
    }

    Ok(GpsLog { sentences, errors })
}

/// Verify the XOR checksum of an NMEA sentence.
///
/// The checksum covers all bytes between `$` and `*` (exclusive).
fn verify_checksum(sentence: &str) -> bool {
    let Some(star_pos) = sentence.find('*') else {
        return false;
    };

    if star_pos < 1 || star_pos + 3 > sentence.len() {
        return false;
    }

    let body = &sentence[1..star_pos];
    let expected_hex = &sentence[star_pos + 1..star_pos + 3];

    let computed: u8 = body.bytes().fold(0u8, |acc, b| acc ^ b);

    let Ok(expected) = u8::from_str_radix(expected_hex, 16) else {
        return false;
    };

    computed == expected
}

/// Parse NMEA latitude/longitude fields into decimal degrees.
///
/// NMEA format: `DDMM.MMMM` for lat, `DDDMM.MMMM` for lon.
fn parse_coordinate(value: &str, hemisphere: &str) -> Option<f64> {
    if value.is_empty() || hemisphere.is_empty() {
        return None;
    }

    let dot_pos = value.find('.')?;
    if dot_pos < 3 {
        return None;
    }

    // Degrees are everything before the last 2 integer digits before the dot
    let deg_end = dot_pos - 2;
    let degrees: f64 = value[..deg_end].parse().ok()?;
    let minutes: f64 = value[deg_end..].parse().ok()?;

    let mut decimal = degrees + minutes / 60.0;

    match hemisphere {
        "S" | "W" => decimal = -decimal,
        "N" | "E" => {}
        _ => return None,
    }

    Some(decimal)
}

/// Parse a `$GPRMC` sentence.
///
/// `$GPRMC,time,status,lat,N/S,lon,E/W,speed,course,date,mag_var,E/W*cs`
fn parse_rmc(fields: &[&str]) -> Option<RmcFix> {
    if fields.len() < 10 {
        return None;
    }

    let utc_time = fields[1].to_owned();
    let valid = fields[2] == "A";

    let position = match (
        parse_coordinate(fields[3], fields[4]),
        parse_coordinate(fields[5], fields[6]),
    ) {
        (Some(lat), Some(lon)) => Some(LatLon {
            latitude: lat,
            longitude: lon,
        }),
        _ => None,
    };

    let speed_knots = fields[7].parse().unwrap_or(0.0);
    let course_degrees = fields[8].parse().unwrap_or(0.0);
    let date = fields[9].to_owned();

    Some(RmcFix {
        utc_time,
        valid,
        position,
        speed_knots,
        course_degrees,
        date,
    })
}

/// Parse a `$GPGGA` sentence.
///
/// `$GPGGA,time,lat,N/S,lon,E/W,quality,sats,hdop,alt,M,geoid,M,age,ref*cs`
fn parse_gga(fields: &[&str]) -> Option<GgaFix> {
    if fields.len() < 10 {
        return None;
    }

    let utc_time = fields[1].to_owned();

    let position = match (
        parse_coordinate(fields[2], fields[3]),
        parse_coordinate(fields[4], fields[5]),
    ) {
        (Some(lat), Some(lon)) => Some(LatLon {
            latitude: lat,
            longitude: lon,
        }),
        _ => None,
    };

    let quality: u8 = fields[6].parse().unwrap_or(0);
    let satellites: u8 = fields[7].parse().unwrap_or(0);
    let hdop: f64 = fields[8].parse().unwrap_or(0.0);
    let altitude_m: f64 = fields[9].parse().unwrap_or(0.0);

    Some(GgaFix {
        utc_time,
        position,
        quality,
        satellites,
        hdop,
        altitude_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_rmc() {
        let sentence = "$GPRMC,143025.000,A,3545.1234,N,08234.5678,W,0.5,45.2,030426,,,A";
        let cs: u8 = sentence[1..].bytes().fold(0u8, |acc, b| acc ^ b);
        let line = format!("{sentence}*{cs:02X}\r\n");

        let log = parse(line.as_bytes()).unwrap();
        assert_eq!(log.sentences.len(), 1);

        let NmeaSentence::Rmc(fix) = &log.sentences[0] else {
            panic!("expected RMC");
        };
        assert!(fix.valid);
        let pos = fix.position.as_ref().expect("should have position");
        assert!((pos.latitude - 35.752_057).abs() < 0.001);
        assert!((pos.longitude - (-82.575_463_333)).abs() < 0.001);
        assert_eq!(fix.date, "030426");
    }

    #[test]
    fn parse_valid_gga() {
        let sentence = "$GPGGA,143025.000,3545.1234,N,08234.5678,W,1,08,1.2,345.6,M,0.0,M,,";
        let cs: u8 = sentence[1..].bytes().fold(0u8, |acc, b| acc ^ b);
        let line = format!("{sentence}*{cs:02X}\r\n");

        let log = parse(line.as_bytes()).unwrap();
        assert_eq!(log.sentences.len(), 1);

        let NmeaSentence::Gga(fix) = &log.sentences[0] else {
            panic!("expected GGA");
        };
        assert_eq!(fix.quality, 1);
        assert_eq!(fix.satellites, 8);
        assert!((fix.altitude_m - 345.6).abs() < 0.01);
    }

    #[test]
    fn checksum_verification() {
        assert!(verify_checksum("$GPGGA,,,,,,,,,*7A"));
        assert!(!verify_checksum("$GPGGA,,,,,,,,,*00"));
    }

    #[test]
    fn empty_file_returns_error() {
        assert!(parse(b"").is_err());
    }

    #[test]
    fn malformed_lines_counted_as_errors() {
        let data = b"$GPRMC,bad,data*FF\r\n$NOTVALID*00\r\n";
        let log = parse(data).unwrap();
        assert!(log.sentences.is_empty());
        assert!(log.errors > 0);
    }

    #[test]
    fn void_rmc_parsed_but_not_valid() {
        let sentence = "$GPRMC,143025.000,V,3545.1234,N,08234.5678,W,0.0,0.0,030426,,,N";
        let cs: u8 = sentence[1..].bytes().fold(0u8, |acc, b| acc ^ b);
        let line = format!("{sentence}*{cs:02X}\r\n");

        let log = parse(line.as_bytes()).unwrap();
        let fixes = log.valid_fixes();
        assert!(fixes.is_empty());
        assert_eq!(log.rmc_fixes().len(), 1);
        assert!(!log.rmc_fixes()[0].valid);
    }

    #[test]
    fn gnrmc_variant_accepted() {
        let sentence = "$GNRMC,120000.000,A,3545.0000,N,08234.0000,W,0.0,0.0,010126,,,A";
        let cs: u8 = sentence[1..].bytes().fold(0u8, |acc, b| acc ^ b);
        let line = format!("{sentence}*{cs:02X}\r\n");

        let log = parse(line.as_bytes()).unwrap();
        assert_eq!(log.sentences.len(), 1);
    }

    #[test]
    fn parse_real_d75_void_fixes() {
        // Real NMEA captured from TH-D75 GPS (indoors, no fix)
        let data = b"\
$GPRMC,,V,,,,,,,,,,N*53\n\
$GPGGA,,,,,,0,,,,,,,,*66\n\
$GPRMC,,V,,,,,,,,,,N*53\n\
$GPGGA,,,,,,0,,,,,,,,*66\n\
$GPRMC,,V,,,,,,,,,,N*53\n\
$GPGGA,,,,,,0,,,,,,,,*66\n";

        let log = parse(data).unwrap();
        // Void RMC has no coordinates — should be skipped by parser
        // GGA with quality=0 has no coordinates — should be skipped
        assert_eq!(log.errors, 0, "checksums should be valid");
        // Void sentences have empty coordinate fields → parse_coordinate returns None
        // so they won't produce Rmc/Gga entries
        let valid = log.valid_fixes();
        assert!(valid.is_empty(), "no valid fixes indoors");
    }

    #[test]
    fn parse_real_d75_live_fix() {
        use std::fmt::Write;

        // Synthetic NMEA matching D75 format (real structure, fake coordinates)
        // Build sentences with valid checksums
        let rmc1 = "$GPRMC,120000.00,A,4052.1234,N,07356.5678,W,2.5,180.0,010126,5.2,E,A";
        let gga1 = "$GPGGA,120000.00,4052.1234,N,07356.5678,W,1,07,1.2,250.5,M,-33.0,M,,";
        let rmc2 = "$GPRMC,120001.00,A,4052.1300,N,07356.5700,W,0.0,0.0,010126,5.2,E,A";
        let gga2 = "$GPGGA,120001.00,4052.1300,N,07356.5700,W,1,05,1.5,250.6,M,-33.0,M,,";

        let mut data = String::new();
        for s in [rmc1, gga1, rmc2, gga2] {
            let cs: u8 = s[1..].bytes().fold(0u8, |acc, b| acc ^ b);
            writeln!(data, "{s}*{cs:02X}").unwrap();
        }

        let log = parse(data.as_bytes()).unwrap();
        assert_eq!(log.errors, 0, "all checksums valid");
        assert_eq!(log.sentences.len(), 4);

        let rmc = log.rmc_fixes();
        assert_eq!(rmc.len(), 2);
        assert!(rmc[0].valid);
        assert_eq!(rmc[0].utc_time, "120000.00");
        assert_eq!(rmc[0].date, "010126");

        let pos = rmc[0].position.as_ref().expect("should have fix");
        // 40°52.1234'N = 40.86872°N
        assert!((pos.latitude - 40.8687).abs() < 0.001);
        // 073°56.5678'W = -73.94280°W
        assert!((pos.longitude - (-73.9428)).abs() < 0.001);
        assert!((rmc[0].speed_knots - 2.5).abs() < 0.1);

        let gga = log.gga_fixes();
        assert_eq!(gga.len(), 2);
        assert_eq!(gga[0].quality, 1);
        assert_eq!(gga[0].satellites, 7);
        assert!((gga[0].altitude_m - 250.5).abs() < 0.1);
    }
}
