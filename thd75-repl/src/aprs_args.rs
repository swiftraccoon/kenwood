//! Argument parsing for APRS-mode commands.
//!
//! Pure functions (no I/O, no radio access), so every grammar rule is
//! unit-testable without hardware. Error strings are printed verbatim
//! by the dispatcher and therefore follow the accessibility phrasing
//! rules: complete sentences, units spelled out, concrete examples.

use kenwood_thd75::{
    CompressedPositionText, Course, Heading, Latitude, Longitude, MiceSpeed, MiceStatusText,
    ObjectName, PositionReportText, Speed,
};

/// A latitude/longitude pair in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLon {
    /// Latitude in decimal degrees, positive north.
    pub latitude: Latitude,
    /// Longitude in decimal degrees, positive east.
    pub longitude: Longitude,
}

/// Parse latitude and longitude strings in decimal degrees.
///
/// # Errors
///
/// Returns a screen-reader-friendly message if either value is not a
/// finite number or is outside latitude ±90 / longitude ±180.
pub fn parse_lat_lon(lat: &str, lon: &str) -> Result<LatLon, String> {
    let lat_deg: f64 = lat.parse().map_err(|_| {
        format!("invalid latitude {lat:?}. Use decimal degrees, for example 35.30.")
    })?;
    let lon_deg: f64 = lon.parse().map_err(|_| {
        format!("invalid longitude {lon:?}. Use decimal degrees, for example -82.46.")
    })?;
    let latitude = Latitude::new(lat_deg)
        .map_err(|_| format!("latitude {lat} is out of range. Use -90 through 90."))?;
    let longitude = Longitude::new(lon_deg)
        .map_err(|_| format!("longitude {lon} is out of range. Use -180 through 180."))?;
    Ok(LatLon {
        latitude,
        longitude,
    })
}

/// Parsed arguments for the uncompressed `position` beacon command.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Free-form comment appended to the beacon (may be empty).
    pub comment: PositionReportText,
}

/// Parse `<latitude> <longitude> [comment...]`.
///
/// # Errors
///
/// Returns a usage message when arguments are missing and a range
/// message for invalid coordinates.
pub fn parse_position(args: &[&str]) -> Result<PositionArgs, String> {
    let [lat, lon, comment @ ..] = args else {
        return Err(
            "usage: <latitude> <longitude> then an optional comment. Example: 35.30 -82.46 Portable."
                .to_string(),
        );
    };
    Ok(PositionArgs {
        pos: parse_lat_lon(lat, lon)?,
        comment: PositionReportText::new(&comment.join(" "))
            .map_err(|error| format!("invalid position text: {error}."))?,
    })
}

/// Parsed arguments for the `compressed` beacon command.
#[derive(Debug, Clone, PartialEq)]
pub struct CompressedPositionArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Text after the compressed position's fixed `csT` bytes.
    pub comment: CompressedPositionText,
}

/// Parse `<latitude> <longitude> [comment...]` for a compressed position.
///
/// # Errors
///
/// Returns a usage message for missing coordinates or an exact APRS text
/// validation error for a comment that cannot fit on air.
pub fn parse_compressed_position(args: &[&str]) -> Result<CompressedPositionArgs, String> {
    let [lat, lon, comment @ ..] = args else {
        return Err(
            "usage: compressed <latitude> <longitude> then an optional comment. Example: compressed 35.30 -82.46 Portable."
                .to_string(),
        );
    };
    Ok(CompressedPositionArgs {
        pos: parse_lat_lon(lat, lon)?,
        comment: CompressedPositionText::new(&comment.join(" "))
            .map_err(|error| format!("invalid compressed-position text: {error}."))?,
    })
}

/// Parsed arguments for the `mice` beacon command.
#[derive(Debug, Clone, PartialEq)]
pub struct MiceArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Ground speed in knots (Mic-E wire range 0 through 799).
    pub speed: MiceSpeed,
    /// Course in degrees. Zero means unknown; 1 through 360 is a heading.
    pub course: Course,
    /// Mic-E status text appended to the beacon (may be empty).
    pub status_text: MiceStatusText,
}

/// Parse `<latitude> <longitude> <speed in knots> <course in degrees> [comment...]`.
///
/// # Errors
///
/// Returns a usage message when arguments are missing, and range
/// messages for coordinates, speed above 799 knots, or course above
/// 360 degrees.
pub fn parse_mice(args: &[&str]) -> Result<MiceArgs, String> {
    let [lat, lon, speed, course, comment @ ..] = args else {
        return Err(
            "usage: mice <latitude> <longitude> <speed in knots> <course in degrees> then an optional comment. Example: mice 35.30 -82.46 25 90 testing."
                .to_string(),
        );
    };
    let pos = parse_lat_lon(lat, lon)?;
    let speed = speed
        .parse::<u16>()
        .ok()
        .and_then(|knots| MiceSpeed::new(knots).ok())
        .ok_or_else(|| format!("invalid speed {speed:?}. Use whole knots from 0 through 799."))?;
    let course = course
        .parse::<u16>()
        .ok()
        .and_then(|degrees| Course::new(degrees).ok())
        .ok_or_else(|| {
            format!(
                "invalid course {course:?}. Use whole degrees from 0 through 360. Zero means unknown."
            )
        })?;
    Ok(MiceArgs {
        pos,
        speed,
        course,
        status_text: MiceStatusText::new(&comment.join(" "))
            .map_err(|error| format!("invalid Mic-E status text: {error}."))?,
    })
}

/// Parsed arguments for the `motion` `SmartBeaconing` command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionArgs {
    /// Current position.
    pub pos: LatLon,
    /// Ground speed in kilometers per hour.
    pub speed: Speed,
    /// Course in degrees, 0 through 360.
    pub heading: Heading,
}

/// Parse `<latitude> <longitude> <speed in km/h> <course in degrees>`.
///
/// # Errors
///
/// Returns a usage message when arguments are missing or out of range.
pub fn parse_motion(args: &[&str]) -> Result<MotionArgs, String> {
    let [lat, lon, speed, course] = args else {
        return Err(
            "usage: motion <latitude> <longitude> <speed in kilometers per hour> <course in degrees>. Example: motion 35.30 -82.46 55 180."
                .to_string(),
        );
    };
    let pos = parse_lat_lon(lat, lon)?;
    let speed = speed
        .parse::<f64>()
        .ok()
        .and_then(|kmh| Speed::from_kmh(kmh).ok())
        .ok_or_else(|| {
            format!("invalid speed {speed:?}. Use kilometers per hour, zero or more.")
        })?;
    let heading = course
        .parse::<f64>()
        .ok()
        .and_then(|degrees| Heading::new(degrees).ok())
        .ok_or_else(|| format!("invalid course {course:?}. Use degrees from 0 through 360."))?;
    Ok(MotionArgs {
        pos,
        speed,
        heading,
    })
}

/// Parsed arguments for the `object` report command.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectArgs {
    /// Validated object name, 1 through 9 printable ASCII bytes.
    pub name: ObjectName,
    /// Object position.
    pub pos: LatLon,
    /// Validated position-report text (may be empty).
    pub comment: PositionReportText,
}

/// Parse `<name> <latitude> <longitude> [comment...]`.
///
/// # Errors
///
/// Returns a usage message when arguments are missing, or a validation
/// message when the object name cannot be represented on air.
pub fn parse_object(args: &[&str]) -> Result<ObjectArgs, String> {
    let [name, lat, lon, comment @ ..] = args else {
        return Err(
            "usage: object <name> <latitude> <longitude> then an optional comment. Example: object CAMP 35.31 -82.45 Field day site."
                .to_string(),
        );
    };
    let name = ObjectName::new(name).map_err(|error| format!("invalid object name: {error}."))?;
    Ok(ObjectArgs {
        name,
        pos: parse_lat_lon(lat, lon)?,
        comment: PositionReportText::new(&comment.join(" "))
            .map_err(|error| format!("invalid object position text: {error}."))?,
    })
}

/// Parsed arguments for `aprs start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartArgs {
    /// Station callsign, upper-cased for the AX.25 wire.
    pub callsign: String,
    /// AX.25 SSID, 0 through 15.
    pub ssid: u8,
    /// Whether to enable the WIDE1-1 fill-in digipeater.
    pub digi: bool,
}

/// Parse `<callsign> [ssid] [digi]`.
///
/// The callsign is upper-cased (AX.25 callsigns are upper-case on the
/// wire, so `aprs start w1aw` behaves like the intended call). The
/// SSID defaults to 0. A trailing `digi` keyword enables the WIDE1-1
/// fill-in digipeater.
///
/// # Errors
///
/// Returns a message when the callsign is missing, the SSID is not a
/// number from 0 through 15, or extra arguments follow.
pub fn parse_start(args: &[&str]) -> Result<StartArgs, String> {
    let Some(raw_callsign) = args.first() else {
        return Err(
            "callsign required. Usage: aprs start <callsign> then an optional SSID and the word digi. Example: aprs start W1AW 7."
                .to_string(),
        );
    };
    let callsign = raw_callsign.to_ascii_uppercase();
    let mut ssid: u8 = 0;
    let mut digi = false;
    let rest = args.get(1..).unwrap_or(&[]);
    match rest {
        [] => {}
        [one] if one.eq_ignore_ascii_case("digi") => digi = true,
        [one] => ssid = parse_ssid(one)?,
        [one, two] if two.eq_ignore_ascii_case("digi") => {
            ssid = parse_ssid(one)?;
            digi = true;
        }
        [_, two] => {
            return Err(format!("unexpected argument {two:?}. Did you mean digi?"));
        }
        _ => {
            return Err(
                "too many arguments. Usage: aprs start <callsign> <ssid> digi.".to_string(),
            );
        }
    }
    Ok(StartArgs {
        callsign,
        ssid,
        digi,
    })
}

/// Parse an SSID token as a number from 0 through 15.
fn parse_ssid(s: &str) -> Result<u8, String> {
    match s.parse::<u8>() {
        Ok(n) if n <= 15 => Ok(n),
        _ => Err(format!(
            "invalid SSID {s:?}. Use a number from 0 through 15."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn lat_lon_parses_and_range_checks() -> TestResult {
        let ok = parse_lat_lon("35.30", "-82.46")?;
        assert!((ok.latitude.as_degrees() - 35.30).abs() < f64::EPSILON);
        assert!((ok.longitude.as_degrees() + 82.46).abs() < f64::EPSILON);
        assert!(parse_lat_lon("91", "0").is_err());
        assert!(parse_lat_lon("0", "-181").is_err());
        assert!(parse_lat_lon("NaN", "0").is_err());
        assert!(parse_lat_lon("north", "0").is_err());
        Ok(())
    }

    #[test]
    fn position_wants_two_coordinates() -> TestResult {
        let p = parse_position(&["35.30", "-82.46", "two", "words"])?;
        assert_eq!(p.comment.as_str(), "two words");
        assert!(parse_position(&["35.30"]).is_err());
        Ok(())
    }

    #[test]
    fn compressed_position_enforces_40_byte_comment_limit() -> TestResult {
        let maximum = "c".repeat(CompressedPositionText::MAX_LEN);
        let parsed = parse_compressed_position(&["35.30", "-82.46", &maximum])?;
        assert_eq!(parsed.comment.as_str(), maximum);

        let too_long = "c".repeat(CompressedPositionText::MAX_LEN + 1);
        assert!(parse_compressed_position(&["35.30", "-82.46", &too_long]).is_err());
        Ok(())
    }

    #[test]
    fn mice_bounds_and_status_text_semantics() -> TestResult {
        let m = parse_mice(&["35.30", "-82.46", "25", "90", "hi"])?;
        assert_eq!((m.speed.as_knots(), m.course.as_degrees()), (25, 90));
        assert_eq!(m.status_text.as_str(), "hi");
        assert!(parse_mice(&["35.30", "-82.46", "800", "90"]).is_err());
        assert!(parse_mice(&["35.30", "-82.46", "25", "361"]).is_err());
        assert!(parse_mice(&["35.30", "-82.46", "25"]).is_err());
        let comma_status = parse_mice(&["35.30", "-82.46", "25", "90", ",ordinary", "status"])?;
        assert_eq!(comma_status.status_text.as_str(), ",ordinary status");
        assert!(parse_mice(&["35.30", "-82.46", "25", "90", "`telemetry"]).is_err());
        Ok(())
    }

    #[test]
    fn motion_takes_floats_and_rejects_negatives() -> TestResult {
        let m = parse_motion(&["35.30", "-82.46", "55.5", "180"])?;
        assert!((m.speed.as_kmh() - 55.5).abs() < f64::EPSILON);
        assert!((m.heading.as_degrees() - 180.0).abs() < f64::EPSILON);
        assert!(parse_motion(&["35.30", "-82.46", "-5", "180"]).is_err());
        assert!(parse_motion(&["35.30", "-82.46", "55"]).is_err());
        Ok(())
    }

    #[test]
    fn object_name_length_is_enforced() -> TestResult {
        let o = parse_object(&["CAMP", "35.31", "-82.45"])?;
        assert_eq!(o.name.as_str(), "CAMP");
        assert!(o.comment.as_str().is_empty());
        assert!(parse_object(&["TENCHARSXX", "35.31", "-82.45"]).is_err());
        assert!(parse_object(&["CAMPé", "35.31", "-82.45"]).is_err());
        assert!(parse_object(&["CAMP\n", "35.31", "-82.45"]).is_err());
        assert!(parse_object(&["CAMP"]).is_err());
        Ok(())
    }

    #[test]
    fn start_grammar_covers_ssid_and_digi() -> TestResult {
        assert_eq!(
            parse_start(&["w1aw"])?,
            StartArgs {
                callsign: "W1AW".to_string(),
                ssid: 0,
                digi: false
            }
        );
        assert_eq!(parse_start(&["W1AW", "7"])?.ssid, 7);
        assert!(parse_start(&["W1AW", "digi"])?.digi);
        let both = parse_start(&["W1AW", "7", "DIGI"])?;
        assert!(both.digi && both.ssid == 7);
        assert!(parse_start(&[]).is_err());
        assert!(parse_start(&["W1AW", "16"]).is_err());
        assert!(parse_start(&["W1AW", "7", "loud"]).is_err());
        Ok(())
    }
}
