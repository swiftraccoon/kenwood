//! Finite positive frame-rate policy for recipient token buckets.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Largest rate whose one-second burst does not exceed a u32 frame count.
const MAXIMUM_RATE: f64 = 4_294_967_295.0;

/// A finite positive rate in frames per second.
///
/// The local upper bound is `u32::MAX` frames/s, matching the token bucket's
/// maximum representable integer burst capacity. This is a resource-policy
/// bound, independent of D-STAR's 50-frame/s voice cadence.
///
/// ```
/// use dstar_gateway_server::FrameRate;
/// let rate = FrameRate::new(60.0)?;
/// assert_eq!(format!("{rate:e}"), "6e1");
/// assert!(FrameRate::new(f64::NAN).is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRate(f64);

impl Eq for FrameRate {}

impl Ord for FrameRate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for FrameRate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for FrameRate {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl FrameRate {
    /// Validate a finite rate within (0, `u32::MAX`] frames per second.
    ///
    /// # Errors
    ///
    /// Returns [`FrameRateError`] for NaN/infinity, zero/negative values, or
    /// rates above the one-second burst-capacity limit.
    pub const fn new(frames_per_second: f64) -> Result<Self, FrameRateError> {
        if !frames_per_second.is_finite()
            || frames_per_second <= 0.0
            || frames_per_second > MAXIMUM_RATE
        {
            return Err(FrameRateError {
                value: frames_per_second,
            });
        }
        Ok(Self(frames_per_second))
    }

    /// Validated frames per second.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for FrameRate {
    type Error = FrameRateError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FrameRate> for f64 {
    fn from(value: FrameRate) -> Self {
        value.get()
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::LowerExp for FrameRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerExp::fmt(&self.0, formatter)
    }
}

impl fmt::UpperExp for FrameRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperExp::fmt(&self.0, formatter)
    }
}

/// A requested recipient rate outside its finite positive capacity domain.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("frame rate must be finite and within (0, {MAXIMUM_RATE}], got {value}")]
pub struct FrameRateError {
    value: f64,
}

impl FrameRateError {
    /// Original invalid frames-per-second value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonfinite_nonpositive_and_unrepresentable_bursts_are_rejected() {
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            -0.0,
            0.0,
            4_294_967_296.0,
        ] {
            assert!(FrameRate::new(value).is_err(), "invalid rate {value:?}");
        }
        for value in [f64::MIN_POSITIVE, 0.05, 50.0, 60.0, f64::from(u32::MAX)] {
            let result = FrameRate::new(value);
            assert!(result.is_ok(), "valid rate {value:?}: {result:?}");
        }
    }
}
