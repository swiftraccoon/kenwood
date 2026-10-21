//! Validated `Latitude` and `Longitude` newtypes.

use super::error::DprsError;

/// Latitude in decimal degrees, in `-90.0..=90.0`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Latitude(f64);

impl Latitude {
    /// Construct a `Latitude` from a decimal-degree value.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::LatitudeOutOfRange`] if `deg` is NaN or
    /// outside `-90.0..=90.0`.
    pub fn new(deg: f64) -> Result<Self, DprsError> {
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
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Longitude(f64);

impl Longitude {
    /// Construct a `Longitude` from a decimal-degree value.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::LongitudeOutOfRange`] if `deg` is NaN or
    /// outside `-180.0..=180.0`.
    pub fn new(deg: f64) -> Result<Self, DprsError> {
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

impl TryFrom<f64> for Latitude {
    type Error = DprsError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Latitude> for f64 {
    fn from(value: Latitude) -> Self {
        value.degrees()
    }
}

impl std::fmt::Display for Latitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::fmt::LowerExp for Latitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerExp::fmt(&self.0, formatter)
    }
}

impl std::fmt::UpperExp for Latitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperExp::fmt(&self.0, formatter)
    }
}

impl TryFrom<f64> for Longitude {
    type Error = DprsError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Longitude> for f64 {
    fn from(value: Longitude) -> Self {
        value.degrees()
    }
}

impl std::fmt::Display for Longitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::fmt::LowerExp for Longitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerExp::fmt(&self.0, formatter)
    }
}

impl std::fmt::UpperExp for Longitude {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperExp::fmt(&self.0, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_accepts_valid() {
        assert!(Latitude::new(0.0).is_ok());
        assert!(Latitude::new(45.5).is_ok());
        assert!(Latitude::new(-45.5).is_ok());
        assert!(Latitude::new(90.0).is_ok());
        assert!(Latitude::new(-90.0).is_ok());
    }

    #[test]
    fn latitude_rejects_out_of_range() {
        assert!(Latitude::new(90.1).is_err());
        assert!(Latitude::new(-90.1).is_err());
        assert!(Latitude::new(f64::NAN).is_err());
        assert!(Latitude::new(f64::INFINITY).is_err());
        assert!(Latitude::new(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn longitude_accepts_valid() {
        assert!(Longitude::new(0.0).is_ok());
        assert!(Longitude::new(180.0).is_ok());
        assert!(Longitude::new(-180.0).is_ok());
        assert!(Longitude::new(0.001).is_ok());
    }

    #[test]
    fn longitude_rejects_out_of_range() {
        assert!(Longitude::new(180.1).is_err());
        assert!(Longitude::new(-180.1).is_err());
        assert!(Longitude::new(f64::NAN).is_err());
    }

    #[test]
    fn degrees_accessor() -> Result<(), Box<dyn std::error::Error>> {
        let lat = Latitude::new(45.5)?;
        assert!((lat.degrees() - 45.5).abs() < f64::EPSILON);
        Ok(())
    }
}
