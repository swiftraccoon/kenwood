//! Validated `Latitude` and `Longitude` newtypes.

use super::error::DprsError;

/// Latitude in decimal degrees, in `-90.0..=90.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Latitude(f64);

impl Latitude {
    /// Construct a `Latitude` from a decimal-degree value.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::LatitudeOutOfRange`] if `deg` is NaN or
    /// outside `-90.0..=90.0`.
    pub fn try_new(deg: f64) -> Result<Self, DprsError> {
        if deg.is_nan() || !(-90.0..=90.0).contains(&deg) {
            return Err(DprsError::LatitudeOutOfRange { got: deg });
        }
        Ok(Self(deg))
    }

    /// Decimal degrees value.
    #[must_use]
    pub const fn degrees(self) -> f64 {
        self.0
    }
}

/// Longitude in decimal degrees, in `-180.0..=180.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Longitude(f64);

impl Longitude {
    /// Construct a `Longitude` from a decimal-degree value.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::LongitudeOutOfRange`] if `deg` is NaN or
    /// outside `-180.0..=180.0`.
    pub fn try_new(deg: f64) -> Result<Self, DprsError> {
        if deg.is_nan() || !(-180.0..=180.0).contains(&deg) {
            return Err(DprsError::LongitudeOutOfRange { got: deg });
        }
        Ok(Self(deg))
    }

    /// Decimal degrees value.
    #[must_use]
    pub const fn degrees(self) -> f64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_accepts_valid() {
        assert!(Latitude::try_new(0.0).is_ok());
        assert!(Latitude::try_new(45.5).is_ok());
        assert!(Latitude::try_new(-45.5).is_ok());
        assert!(Latitude::try_new(90.0).is_ok());
        assert!(Latitude::try_new(-90.0).is_ok());
    }

    #[test]
    fn latitude_rejects_out_of_range() {
        assert!(Latitude::try_new(90.1).is_err());
        assert!(Latitude::try_new(-90.1).is_err());
        assert!(Latitude::try_new(f64::NAN).is_err());
        assert!(Latitude::try_new(f64::INFINITY).is_err());
        assert!(Latitude::try_new(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn longitude_accepts_valid() {
        assert!(Longitude::try_new(0.0).is_ok());
        assert!(Longitude::try_new(180.0).is_ok());
        assert!(Longitude::try_new(-180.0).is_ok());
        assert!(Longitude::try_new(0.001).is_ok());
    }

    #[test]
    fn longitude_rejects_out_of_range() {
        assert!(Longitude::try_new(180.1).is_err());
        assert!(Longitude::try_new(-180.1).is_err());
        assert!(Longitude::try_new(f64::NAN).is_err());
    }

    #[test]
    fn degrees_accessor() -> Result<(), Box<dyn std::error::Error>> {
        let lat = Latitude::try_new(45.5)?;
        assert!((lat.degrees() - 45.5).abs() < f64::EPSILON);
        Ok(())
    }
}
