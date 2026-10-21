//! Checked APRS command arguments parsed before radio mode changes.
//!
//! Parsers accept token slices from the REPL's whitespace grammar. Text tokens
//! are joined with one space; quoted or embedded whitespace is not preserved.
//! Errors identify the argument and retain numeric or domain-validation causes.

use std::error::Error;
use std::fmt;

use kenwood_thd75::types::TncDataBand;
use kenwood_thd75::{
    Ax25Address, CompressedPositionText, Course, Heading, Latitude, Longitude, MiceSpeed,
    MiceStatusText, ObjectName, PositionReportText, Speed,
};

/// Command argument whose token failed parsing or domain validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Argument {
    /// Latitude in decimal degrees.
    Latitude,
    /// Longitude in decimal degrees.
    Longitude,
    /// Ground speed in the command's documented units.
    Speed,
    /// Course or heading in degrees.
    Course,
    /// Position or status text.
    Text,
    /// APRS object name.
    ObjectName,
    /// AX.25 station address.
    Address,
    /// AX.25 secondary station identifier.
    Ssid,
    /// TNC data-band selector.
    DataBand,
    /// Optional command keyword.
    Keyword,
}

impl fmt::Display for Argument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Latitude => "latitude",
            Self::Longitude => "longitude",
            Self::Speed => "speed",
            Self::Course => "course",
            Self::Text => "text",
            Self::ObjectName => "object name",
            Self::Address => "station address",
            Self::Ssid => "SSID",
            Self::DataBand => "TNC data band",
            Self::Keyword => "keyword",
        })
    }
}

/// Argument syntax or validated-domain failure.
#[derive(Debug)]
#[non_exhaustive]
pub enum ArgumentError {
    /// Required tokens are missing or extra tokens follow the grammar.
    Usage {
        /// Accepted command grammar.
        usage: &'static str,
    },
    /// A numeric parser or checked domain rejected an argument.
    Invalid {
        /// Argument with invalid input.
        argument: Argument,
        /// Supplied token or joined text.
        value: String,
        /// Original numeric or domain-validation error.
        source: Box<dyn Error + Send + Sync>,
    },
    /// A selector or keyword is outside its accepted vocabulary.
    Token {
        /// Argument with invalid input.
        argument: Argument,
        /// Supplied token.
        value: String,
        /// Accepted vocabulary.
        expected: &'static str,
    },
}

impl ArgumentError {
    fn invalid(
        argument: Argument,
        value: &str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self::Invalid {
            argument,
            value: value.to_owned(),
            source: Box::new(source),
        }
    }

    fn token(argument: Argument, value: &str, expected: &'static str) -> Self {
        Self::Token {
            argument,
            value: value.to_owned(),
            expected,
        }
    }
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage { usage } => write!(f, "usage: {usage}."),
            Self::Invalid {
                argument,
                value,
                source,
            } => {
                write!(f, "invalid {argument} {value:?}: {source}.")
            }
            Self::Token {
                argument,
                value,
                expected,
            } => {
                write!(f, "invalid {argument} {value:?}. Use {expected}.")
            }
        }
    }
}

impl Error for ArgumentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid { source, .. } => Some(source.as_ref()),
            Self::Usage { .. } | Self::Token { .. } => None,
        }
    }
}

/// A checked geographic coordinate pair in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatLon {
    /// Latitude, positive north.
    pub latitude: Latitude,
    /// Longitude, positive east.
    pub longitude: Longitude,
}

impl LatLon {
    /// Parse finite decimal degrees within latitude ±90 and longitude ±180.
    ///
    /// # Errors
    ///
    /// Returns an argument-specific numeric or range failure.
    ///
    /// ```
    /// use thd75_repl::aprs_args::LatLon;
    /// let position = LatLon::parse("35.30", "-82.46")?;
    /// assert_eq!(position.latitude.as_degrees(), 35.30);
    /// # Ok::<(), thd75_repl::aprs_args::ArgumentError>(())
    /// ```
    pub fn parse(lat: &str, lon: &str) -> Result<Self, ArgumentError> {
        let lat_degrees = lat
            .parse::<f64>()
            .map_err(|error| ArgumentError::invalid(Argument::Latitude, lat, error))?;
        let lon_degrees = lon
            .parse::<f64>()
            .map_err(|error| ArgumentError::invalid(Argument::Longitude, lon, error))?;
        Ok(Self {
            latitude: Latitude::new(lat_degrees)
                .map_err(|error| ArgumentError::invalid(Argument::Latitude, lat, error))?,
            longitude: Longitude::new(lon_degrees)
                .map_err(|error| ArgumentError::invalid(Argument::Longitude, lon, error))?,
        })
    }
}

/// Checked uncompressed-position beacon arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Position-report text, possibly empty.
    pub comment: PositionReportText,
}

impl PositionArgs {
    /// Parse latitude, longitude and optional space-joined text.
    ///
    /// # Errors
    ///
    /// Returns usage, coordinate or position-text validation errors.
    /// See the [coordinate example](LatLon::parse).
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [lat, lon, comment @ ..] = args else {
            return Err(ArgumentError::Usage {
                usage: "position <latitude> <longitude> [comment]",
            });
        };
        let text = comment.join(" ");
        Ok(Self {
            pos: LatLon::parse(lat, lon)?,
            comment: PositionReportText::new(&text)
                .map_err(|error| ArgumentError::invalid(Argument::Text, &text, error))?,
        })
    }
}

/// Checked compressed-position beacon arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedPositionArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Text after the compressed position's fixed csT bytes.
    pub comment: CompressedPositionText,
}

impl CompressedPositionArgs {
    /// Parse latitude, longitude and optional space-joined compressed text.
    ///
    /// # Errors
    ///
    /// Returns usage, coordinate or compressed-text validation errors.
    /// See the [coordinate example](LatLon::parse).
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [lat, lon, comment @ ..] = args else {
            return Err(ArgumentError::Usage {
                usage: "compressed <latitude> <longitude> [comment]",
            });
        };
        let text = comment.join(" ");
        Ok(Self {
            pos: LatLon::parse(lat, lon)?,
            comment: CompressedPositionText::new(&text)
                .map_err(|error| ArgumentError::invalid(Argument::Text, &text, error))?,
        })
    }
}

/// Checked Mic-E beacon arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiceArgs {
    /// Beacon position.
    pub pos: LatLon,
    /// Speed in knots, 0 through 799.
    pub speed: MiceSpeed,
    /// Course in degrees; zero means unknown.
    pub course: Course,
    /// Mic-E status text, possibly empty.
    pub status_text: MiceStatusText,
}

impl MiceArgs {
    /// Parse latitude, longitude, whole knots, whole degrees and optional text.
    ///
    /// # Errors
    ///
    /// Returns usage or typed coordinate, numeric, speed, course or text errors.
    /// See the [coordinate example](LatLon::parse).
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [lat, lon, speed, course, comment @ ..] = args else {
            return Err(ArgumentError::Usage {
                usage: "mice <latitude> <longitude> <knots> <degrees> [comment]",
            });
        };
        let knots = speed
            .parse::<u16>()
            .map_err(|error| ArgumentError::invalid(Argument::Speed, speed, error))?;
        let degrees = course
            .parse::<u16>()
            .map_err(|error| ArgumentError::invalid(Argument::Course, course, error))?;
        let text = comment.join(" ");
        Ok(Self {
            pos: LatLon::parse(lat, lon)?,
            speed: MiceSpeed::new(knots)
                .map_err(|error| ArgumentError::invalid(Argument::Speed, speed, error))?,
            course: Course::new(degrees)
                .map_err(|error| ArgumentError::invalid(Argument::Course, course, error))?,
            status_text: MiceStatusText::new(&text)
                .map_err(|error| ArgumentError::invalid(Argument::Text, &text, error))?,
        })
    }
}

/// Checked `SmartBeaconing` motion arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionArgs {
    /// Current position.
    pub pos: LatLon,
    /// Ground speed in kilometers per hour.
    pub speed: Speed,
    /// Heading in degrees.
    pub heading: Heading,
}

impl MotionArgs {
    /// Parse latitude, longitude, kilometers per hour and heading.
    ///
    /// # Errors
    ///
    /// Returns usage or typed coordinate, numeric, speed or heading errors.
    /// See the [coordinate example](LatLon::parse).
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [lat, lon, speed, course] = args else {
            return Err(ArgumentError::Usage {
                usage: "motion <latitude> <longitude> <kilometers per hour> <degrees>",
            });
        };
        let kmh = speed
            .parse::<f64>()
            .map_err(|error| ArgumentError::invalid(Argument::Speed, speed, error))?;
        let degrees = course
            .parse::<f64>()
            .map_err(|error| ArgumentError::invalid(Argument::Course, course, error))?;
        Ok(Self {
            pos: LatLon::parse(lat, lon)?,
            speed: Speed::from_kmh(kmh)
                .map_err(|error| ArgumentError::invalid(Argument::Speed, speed, error))?,
            heading: Heading::new(degrees)
                .map_err(|error| ArgumentError::invalid(Argument::Course, course, error))?,
        })
    }
}

/// Checked APRS object-report arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectArgs {
    /// Validated object name.
    pub name: ObjectName,
    /// Object position.
    pub pos: LatLon,
    /// Position-report text, possibly empty.
    pub comment: PositionReportText,
}

impl ObjectArgs {
    /// Parse an object name, latitude, longitude and optional text.
    ///
    /// # Errors
    ///
    /// Returns usage or typed name, coordinate or position-text errors.
    /// See the [coordinate example](LatLon::parse).
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [name, lat, lon, comment @ ..] = args else {
            return Err(ArgumentError::Usage {
                usage: "object <name> <latitude> <longitude> [comment]",
            });
        };
        let text = comment.join(" ");
        Ok(Self {
            name: ObjectName::new(name)
                .map_err(|error| ArgumentError::invalid(Argument::ObjectName, name, error))?,
            pos: LatLon::parse(lat, lon)?,
            comment: PositionReportText::new(&text)
                .map_err(|error| ArgumentError::invalid(Argument::Text, &text, error))?,
        })
    }
}

/// Validated station identity and KISS routing for APRS startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartArgs {
    address: Ax25Address,
    data_band: TncDataBand,
    digi: bool,
}

impl StartArgs {
    /// Parse a base callsign, mandatory A/B band, optional SSID and digi keyword.
    ///
    /// ASCII callsign letters are uppercased before AX.25 validation. The SSID
    /// defaults to zero; a callsign containing a separate SSID suffix is rejected.
    ///
    /// # Errors
    ///
    /// Returns usage, unknown-token, integer or checked address errors before
    /// any radio mode change.
    ///
    /// ```
    /// use thd75_repl::aprs_args::StartArgs;
    /// let start = StartArgs::parse(&["w1aw", "b", "7", "digi"])?;
    /// assert_eq!(start.address().to_string(), "W1AW-7");
    /// assert!(start.digipeater());
    /// # Ok::<(), thd75_repl::aprs_args::ArgumentError>(())
    /// ```
    pub fn parse(args: &[&str]) -> Result<Self, ArgumentError> {
        let [callsign, band, rest @ ..] = args else {
            return Err(ArgumentError::Usage {
                usage: "aprs start <callsign> <a or b> [ssid] [digi]",
            });
        };
        let data_band = if band.eq_ignore_ascii_case("a") {
            TncDataBand::A
        } else if band.eq_ignore_ascii_case("b") {
            TncDataBand::B
        } else {
            return Err(ArgumentError::token(Argument::DataBand, band, "a or b"));
        };
        let (ssid, digi) = match rest {
            [] => (0, false),
            [keyword] if keyword.eq_ignore_ascii_case("digi") => (0, true),
            [ssid] => (parse_ssid(ssid)?, false),
            [ssid, keyword] if keyword.eq_ignore_ascii_case("digi") => (parse_ssid(ssid)?, true),
            [_, keyword] => {
                return Err(ArgumentError::token(Argument::Keyword, keyword, "digi"));
            }
            _ => {
                return Err(ArgumentError::Usage {
                    usage: "aprs start <callsign> <a or b> [ssid] [digi]",
                });
            }
        };
        let address = Ax25Address::new(&callsign.to_ascii_uppercase(), ssid)
            .map_err(|error| ArgumentError::invalid(Argument::Address, callsign, error))?;
        Ok(Self {
            address,
            data_band,
            digi,
        })
    }

    /// Complete checked AX.25 station address.
    #[must_use]
    pub const fn address(&self) -> &Ax25Address {
        &self.address
    }

    /// TNC data band for the one KISS transition.
    #[must_use]
    pub const fn data_band(&self) -> TncDataBand {
        self.data_band
    }

    /// Whether WIDE1-1 fill-in digipeating was requested.
    #[must_use]
    pub const fn digipeater(&self) -> bool {
        self.digi
    }

    /// Transfer the checked station address to its client configuration.
    #[must_use]
    pub fn into_address(self) -> Ax25Address {
        self.address
    }
}

fn parse_ssid(value: &str) -> Result<u8, ArgumentError> {
    value
        .parse::<u8>()
        .map_err(|error| ArgumentError::invalid(Argument::Ssid, value, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn coordinates_reject_nonfinite_and_outside_values_before_rounding() -> TestResult {
        let position = LatLon::parse("35.30", "-82.46")?;
        assert_eq!(position.latitude, Latitude::new(35.30)?);
        for latitude in ["NaN", "inf", "91", "-90.000000001", "north"] {
            assert!(LatLon::parse(latitude, "0").is_err(), "{latitude}");
        }
        for longitude in ["NaN", "-inf", "-181", "180.000000001", "east"] {
            assert!(LatLon::parse("0", longitude).is_err(), "{longitude}");
        }
        Ok(())
    }

    #[test]
    fn startup_rejects_invalid_identity_band_ssid_and_extra_tokens() -> TestResult {
        let start = StartArgs::parse(&["w1aw", "B", "7", "DIGI"])?;
        assert_eq!(start.address(), &Ax25Address::new("W1AW", 7)?);
        assert_eq!(start.data_band(), TncDataBand::B);
        assert!(start.digipeater());
        assert_eq!(
            StartArgs::parse(&["W1AW", "a"])?.address(),
            &Ax25Address::new("W1AW", 0)?
        );
        for invalid in ["", "TOOLONG", "W1-AW", "W1AW-7", "W1AW\n", "é"] {
            assert!(StartArgs::parse(&[invalid, "a"]).is_err(), "{invalid:?}");
        }
        for args in [
            vec![],
            vec!["W1AW"],
            vec!["W1AW", "c"],
            vec!["W1AW", "a", "16"],
            vec!["W1AW", "a", "seven"],
            vec!["W1AW", "a", "7", "loud"],
            vec!["W1AW", "a", "7", "digi", "extra"],
        ] {
            assert!(StartArgs::parse(&args).is_err(), "{args:?}");
        }
        Ok(())
    }

    #[test]
    fn parse_error_retains_numeric_and_domain_causes() {
        let numeric = LatLon::parse("north", "0");
        assert!(
            matches!(numeric, Err(ArgumentError::Invalid {
            argument: Argument::Latitude, ref source, ..
        }) if source.downcast_ref::<std::num::ParseFloatError>().is_some()),
            "{numeric:?}"
        );
        let domain = StartArgs::parse(&["W1-AW", "a"]);
        assert!(
            matches!(domain, Err(ArgumentError::Invalid {
            argument: Argument::Address, ref source, ..
        }) if source.downcast_ref::<kenwood_thd75::Ax25Error>().is_some()),
            "{domain:?}"
        );
    }

    #[test]
    fn beacon_text_motion_and_object_domains_are_checked() -> TestResult {
        let position = PositionArgs::parse(&["35.30", "-82.46", "two", "words"])?;
        assert_eq!(position.comment.as_str(), "two words");
        assert!(PositionArgs::parse(&["35.30"]).is_err());
        let maximum = "c".repeat(CompressedPositionText::MAX_LEN);
        assert_eq!(
            CompressedPositionArgs::parse(&["35.30", "-82.46", &maximum])?
                .comment
                .as_str(),
            maximum
        );
        assert!(
            CompressedPositionArgs::parse(&["35.30", "-82.46", &format!("{maximum}c")]).is_err()
        );
        let mice = MiceArgs::parse(&["35.30", "-82.46", "25", "90", ",ordinary", "status"])?;
        assert_eq!(mice.speed.as_knots(), 25);
        assert_eq!(mice.status_text.as_str(), ",ordinary status");
        assert!(MiceArgs::parse(&["35.30", "-82.46", "800", "90"]).is_err());
        assert!(MiceArgs::parse(&["35.30", "-82.46", "25", "361"]).is_err());
        assert!(MiceArgs::parse(&["35.30", "-82.46", "25", "90", "`telemetry"]).is_err());
        let motion = MotionArgs::parse(&["35.30", "-82.46", "55.5", "180"])?;
        assert_eq!(motion.speed, Speed::from_kmh(55.5)?);
        assert!(MotionArgs::parse(&["35.30", "-82.46", "-5", "180"]).is_err());
        assert!(MotionArgs::parse(&["35.30", "-82.46", "55", "NaN"]).is_err());
        let object = ObjectArgs::parse(&["CAMP", "35.31", "-82.45"])?;
        assert_eq!(object.name.as_str(), "CAMP");
        for name in ["TENCHARSXX", "CAMPé", "CAMP\n"] {
            assert!(ObjectArgs::parse(&[name, "35.31", "-82.45"]).is_err());
        }
        Ok(())
    }
}
