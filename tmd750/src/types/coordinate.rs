//! Geographic positions as the radio stores them.
//!
//! Repeater records and the GPS position memory store an angle per axis as a
//! degree byte, a minute byte and a little-endian `u16` of ten-thousandths of
//! a minute, with each hemisphere in a separate flag bit.

use crate::error::{SettingName, ValidationError};

/// An angle as whole degrees, whole minutes and ten-thousandths of a minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coordinate {
    degrees: u8,
    minutes: u8,
    ten_thousandths: u16,
}

impl Coordinate {
    /// Validate the minute fields.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] for minutes above 59 or
    /// ten-thousandths above 9999.
    pub const fn new(
        degrees: u8,
        minutes: u8,
        ten_thousandths: u16,
    ) -> Result<Self, ValidationError> {
        if minutes > 59 {
            return Err(ValidationError::SettingOutOfRange {
                setting: SettingName::PositionMinutes,
                value: minutes as u64,
                max: 59,
            });
        }
        if ten_thousandths > 9999 {
            return Err(ValidationError::SettingOutOfRange {
                setting: SettingName::PositionMinuteFraction,
                value: ten_thousandths as u64,
                max: 9999,
            });
        }
        Ok(Self {
            degrees,
            minutes,
            ten_thousandths,
        })
    }

    /// Whole degrees.
    #[must_use]
    pub const fn degrees(self) -> u8 {
        self.degrees
    }

    /// Whole minutes, 0 to 59.
    #[must_use]
    pub const fn minutes(self) -> u8 {
        self.minutes
    }

    /// Ten-thousandths of a minute, 0 to 9999.
    #[must_use]
    pub const fn ten_thousandths(self) -> u16 {
        self.ten_thousandths
    }

    /// The angle in decimal degrees.
    fn magnitude(self) -> f64 {
        let minutes = f64::from(self.minutes) + f64::from(self.ten_thousandths) / 10_000.0;
        f64::from(self.degrees) + minutes / 60.0
    }

    /// Whether the angle exceeds `limit` whole degrees.
    const fn exceeds(self, limit: u8) -> bool {
        self.degrees > limit
            || (self.degrees == limit && (self.minutes > 0 || self.ten_thousandths > 0))
    }

    /// The four stored bytes, taken without validation.
    pub(crate) const fn from_stored(stored: [u8; 4]) -> Self {
        let [degrees, minutes, low, high] = stored;
        Self {
            degrees,
            minutes,
            ten_thousandths: u16::from_le_bytes([low, high]),
        }
    }

    /// The four stored bytes.
    pub(crate) const fn to_stored(self) -> [u8; 4] {
        let [low, high] = self.ten_thousandths.to_le_bytes();
        [self.degrees, self.minutes, low, high]
    }
}

/// North or south of the equator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatitudeHemisphere {
    /// North of the equator.
    North,
    /// South of the equator.
    South,
}

/// East or west of Greenwich.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LongitudeHemisphere {
    /// East of Greenwich.
    East,
    /// West of Greenwich.
    West,
}

/// A latitude of at most 90 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Latitude {
    hemisphere: LatitudeHemisphere,
    coordinate: Coordinate,
}

impl Latitude {
    /// Validate the angle.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] beyond 90 degrees.
    pub const fn new(
        hemisphere: LatitudeHemisphere,
        coordinate: Coordinate,
    ) -> Result<Self, ValidationError> {
        if coordinate.exceeds(90) {
            return Err(ValidationError::SettingOutOfRange {
                setting: SettingName::LatitudeDegrees,
                value: coordinate.degrees as u64,
                max: 90,
            });
        }
        Ok(Self {
            hemisphere,
            coordinate,
        })
    }

    /// A stored latitude, taken without validation.
    pub(crate) const fn from_stored(south: bool, stored: [u8; 4]) -> Self {
        Self {
            hemisphere: if south {
                LatitudeHemisphere::South
            } else {
                LatitudeHemisphere::North
            },
            coordinate: Coordinate::from_stored(stored),
        }
    }

    /// North or south.
    #[must_use]
    pub const fn hemisphere(self) -> LatitudeHemisphere {
        self.hemisphere
    }

    /// The angle from the equator.
    #[must_use]
    pub const fn coordinate(self) -> Coordinate {
        self.coordinate
    }

    /// Signed decimal degrees, negative to the south.
    #[must_use]
    pub fn degrees(self) -> f64 {
        match self.hemisphere {
            LatitudeHemisphere::North => self.coordinate.magnitude(),
            LatitudeHemisphere::South => -self.coordinate.magnitude(),
        }
    }
}

/// A longitude of at most 180 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Longitude {
    hemisphere: LongitudeHemisphere,
    coordinate: Coordinate,
}

impl Longitude {
    /// Validate the angle.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] beyond 180 degrees.
    pub const fn new(
        hemisphere: LongitudeHemisphere,
        coordinate: Coordinate,
    ) -> Result<Self, ValidationError> {
        if coordinate.exceeds(180) {
            return Err(ValidationError::SettingOutOfRange {
                setting: SettingName::LongitudeDegrees,
                value: coordinate.degrees as u64,
                max: 180,
            });
        }
        Ok(Self {
            hemisphere,
            coordinate,
        })
    }

    /// A stored longitude, taken without validation.
    pub(crate) const fn from_stored(west: bool, stored: [u8; 4]) -> Self {
        Self {
            hemisphere: if west {
                LongitudeHemisphere::West
            } else {
                LongitudeHemisphere::East
            },
            coordinate: Coordinate::from_stored(stored),
        }
    }

    /// East or west.
    #[must_use]
    pub const fn hemisphere(self) -> LongitudeHemisphere {
        self.hemisphere
    }

    /// The angle from Greenwich.
    #[must_use]
    pub const fn coordinate(self) -> Coordinate {
        self.coordinate
    }

    /// Signed decimal degrees, negative to the west.
    #[must_use]
    pub fn degrees(self) -> f64 {
        match self.hemisphere {
            LongitudeHemisphere::East => self.coordinate.magnitude(),
            LongitudeHemisphere::West => -self.coordinate.magnitude(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn angles_bound_their_minutes_and_degrees() -> TestResult {
        assert!(Coordinate::new(0, 60, 0).is_err());
        assert!(Coordinate::new(0, 59, 10_000).is_err());
        assert!(Latitude::new(LatitudeHemisphere::North, Coordinate::new(90, 0, 1)?).is_err());
        assert!(Latitude::new(LatitudeHemisphere::North, Coordinate::new(90, 0, 0)?).is_ok());
        assert!(Longitude::new(LongitudeHemisphere::East, Coordinate::new(180, 0, 1)?).is_err());
        assert!(Longitude::new(LongitudeHemisphere::East, Coordinate::new(180, 0, 0)?).is_ok());
        Ok(())
    }

    #[test]
    fn stored_angles_round_trip_with_signed_degrees() -> TestResult {
        let stored = [35, 41, 0xE4, 0x25];
        let latitude = Latitude::from_stored(true, stored);
        assert_eq!(latitude.coordinate(), Coordinate::new(35, 41, 9700)?);
        assert_eq!(latitude.coordinate().to_stored(), stored);
        assert!((latitude.degrees() + 35.6995).abs() < 1e-9);
        let longitude = Longitude::from_stored(false, [139, 46, 0x60, 0x09]);
        assert_eq!(longitude.hemisphere(), LongitudeHemisphere::East);
        assert!((longitude.degrees() - 139.770_667).abs() < 1e-6);
        Ok(())
    }
}
