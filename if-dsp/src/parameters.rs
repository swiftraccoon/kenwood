//! Validated scalar domains shared by the DSP stages.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Numeric parameter rejected by a checked constructor.
///
/// See the [configuration example](crate#configure-and-process-an-if-stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DspParameter {
    /// Samples per second, finite and greater than zero.
    SampleRate,
    /// Signed oscillator or passband frequency, finite.
    Frequency,
    /// Filter width or cutoff in hertz, finite and greater than zero.
    Bandwidth,
    /// AGC target amplitude, finite and greater than zero.
    Target,
    /// AGC attack time in milliseconds, finite and greater than zero.
    AttackTime,
    /// AGC decay time in milliseconds, finite and greater than zero.
    DecayTime,
    /// AGC gain ceiling, finite and greater than zero.
    MaximumGain,
}

impl fmt::Display for DspParameter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SampleRate => "sample rate in hertz",
            Self::Frequency => "frequency in hertz",
            Self::Bandwidth => "bandwidth in hertz",
            Self::Target => "AGC target amplitude",
            Self::AttackTime => "AGC attack time in milliseconds",
            Self::DecayTime => "AGC decay time in milliseconds",
            Self::MaximumGain => "AGC maximum gain",
        })
    }
}

/// Invalid DSP metadata, coefficient, or combination of otherwise valid values.
///
/// No allocation failure is represented here; see
/// [allocation and samples](crate#allocation-and-samples).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DspError {
    /// A scalar is nonfinite, or a positive parameter is zero or negative.
    InvalidParameter {
        /// Rejected domain.
        parameter: DspParameter,
        /// Supplied value in the parameter's documented units.
        value: f64,
    },
    /// A windowed-sinc tap count is even or below three.
    InvalidTapCount {
        /// Supplied number of coefficients.
        count: usize,
    },
    /// A rate-conversion factor is zero.
    InvalidRateFactor,
    /// An FFT size is below two, so an overlapping segment cannot advance.
    InvalidFftSize {
        /// Supplied number of samples per segment.
        size: usize,
    },
    /// A cutoff reaches or exceeds the applicable Nyquist limit.
    InvalidCutoff {
        /// Supplied cutoff in hertz.
        cutoff_hz: f64,
        /// Exclusive upper bound in hertz.
        maximum_hz: f64,
    },
    /// A band is unordered or reaches either Nyquist boundary.
    InvalidPassband {
        /// Supplied lower edge in hertz.
        low_hz: f64,
        /// Supplied upper edge in hertz.
        high_hz: f64,
        /// Samples per second used to interpret the edges.
        sample_rate_hz: f32,
    },
    /// A streaming FIR has no coefficients or contains a nonfinite coefficient.
    InvalidTaps {
        /// First nonfinite coefficient; `None` means the vector was empty.
        index: Option<usize>,
    },
    /// A rate and time constant cannot produce a positive tracking coefficient.
    InvalidAgcTiming {
        /// Samples per second.
        sample_rate_hz: f32,
        /// Rejected attack or decay time in milliseconds.
        milliseconds: f32,
    },
}

impl fmt::Display for DspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter { parameter, value } => {
                write!(formatter, "invalid {parameter}: {value}")
            }
            Self::InvalidTapCount { count } => {
                write!(
                    formatter,
                    "tap count {count} must be odd and at least three"
                )
            }
            Self::InvalidRateFactor => formatter.write_str("rate factor must be positive"),
            Self::InvalidFftSize { size } => {
                write!(formatter, "FFT size {size} must be at least two")
            }
            Self::InvalidCutoff {
                cutoff_hz,
                maximum_hz,
            } => {
                write!(
                    formatter,
                    "cutoff {cutoff_hz} Hz must be below {maximum_hz} Hz"
                )
            }
            Self::InvalidPassband {
                low_hz,
                high_hz,
                sample_rate_hz,
            } => {
                write!(
                    formatter,
                    "invalid band {low_hz}..{high_hz} Hz at {sample_rate_hz} samples/s"
                )
            }
            Self::InvalidTaps { index: Some(index) } => {
                write!(formatter, "coefficient {index} is not finite")
            }
            Self::InvalidTaps { index: None } => formatter.write_str("FIR coefficients are empty"),
            Self::InvalidAgcTiming {
                sample_rate_hz,
                milliseconds,
            } => {
                write!(
                    formatter,
                    "AGC time {milliseconds} ms has no positive tracking coefficient at {sample_rate_hz} samples/s"
                )
            }
        }
    }
}

impl std::error::Error for DspError {}

/// A finite positive sample rate in samples per second.
///
/// See the [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleRate(f32);

impl SampleRate {
    /// Check a sample rate, including subnormal positive values.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub const fn new(hz: f32) -> Result<Self, DspError> {
        if hz.is_finite() && hz > 0.0 {
            Ok(Self(hz))
        } else {
            Err(DspError::InvalidParameter {
                parameter: DspParameter::SampleRate,
                value: hz as f64,
            })
        }
    }

    /// Samples per second.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for SampleRate {
    type Error = DspError;
    fn try_from(hz: f32) -> Result<Self, Self::Error> {
        Self::new(hz)
    }
}

impl From<SampleRate> for f32 {
    fn from(rate: SampleRate) -> Self {
        rate.get()
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Eq for SampleRate {}

impl PartialOrd for SampleRate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SampleRate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for SampleRate {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// A finite signed frequency in hertz.
///
/// Oscillators interpret aliases modulo their sample rate; filter designs
/// additionally check their Nyquist boundaries. Signed zero is stored as
/// positive zero so equality, ordering and hashing agree. See the
/// [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frequency(f64);

impl Frequency {
    /// Check a signed frequency; zero is a valid DC frequency.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for NaN or either infinity.
    pub const fn new(hz: f64) -> Result<Self, DspError> {
        if hz.is_finite() {
            Ok(Self(if hz == 0.0 { 0.0 } else { hz }))
        } else {
            Err(DspError::InvalidParameter {
                parameter: DspParameter::Frequency,
                value: hz,
            })
        }
    }

    /// Signed hertz.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Frequency {
    type Error = DspError;
    fn try_from(hz: f64) -> Result<Self, Self::Error> {
        Self::new(hz)
    }
}

impl From<Frequency> for f64 {
    fn from(frequency: Frequency) -> Self {
        frequency.get()
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Eq for Frequency {}

impl PartialOrd for Frequency {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Frequency {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for Frequency {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// A finite positive filter width or cutoff in hertz.
///
/// A design also checks this width against its sample rate. See the
/// [configuration example](crate#configure-and-process-an-if-stream).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bandwidth(f32);

impl Bandwidth {
    /// Check a positive width without imposing a particular sample rate.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub const fn new(hz: f32) -> Result<Self, DspError> {
        if hz.is_finite() && hz > 0.0 {
            Ok(Self(hz))
        } else {
            Err(DspError::InvalidParameter {
                parameter: DspParameter::Bandwidth,
                value: hz as f64,
            })
        }
    }

    /// Width in hertz.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for Bandwidth {
    type Error = DspError;
    fn try_from(hz: f32) -> Result<Self, Self::Error> {
        Self::new(hz)
    }
}

impl From<Bandwidth> for f32 {
    fn from(width: Bandwidth) -> Self {
        width.get()
    }
}

impl fmt::Display for Bandwidth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Eq for Bandwidth {}

impl PartialOrd for Bandwidth {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bandwidth {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for Bandwidth {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// An odd windowed-sinc coefficient count of at least three.
///
/// Allocation limits remain the caller's responsibility. See the
/// [filter design example](crate#design-and-stream-a-filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TapCount(usize);

impl TapCount {
    /// Check an odd count whose Hamming window has a nonzero denominator.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTapCount`] for zero, one, or an even count.
    pub const fn new(count: usize) -> Result<Self, DspError> {
        if count >= 3 && count % 2 == 1 {
            Ok(Self(count))
        } else {
            Err(DspError::InvalidTapCount { count })
        }
    }

    /// Number of coefficients.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for TapCount {
    type Error = DspError;
    fn try_from(count: usize) -> Result<Self, Self::Error> {
        Self::new(count)
    }
}

impl From<TapCount> for usize {
    fn from(count: TapCount) -> Self {
        count.get()
    }
}

impl fmt::Display for TapCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A positive integer rate-conversion factor.
///
/// See the [rate conversion example](crate#convert-sample-rates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RateFactor(usize);

impl RateFactor {
    /// Reject a zero rate-conversion factor.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidRateFactor`] when `factor` is zero.
    pub const fn new(factor: usize) -> Result<Self, DspError> {
        if factor > 0 {
            Ok(Self(factor))
        } else {
            Err(DspError::InvalidRateFactor)
        }
    }

    /// Integer ratio of the two rates.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for RateFactor {
    type Error = DspError;
    fn try_from(factor: usize) -> Result<Self, Self::Error> {
        Self::new(factor)
    }
}

impl From<RateFactor> for usize {
    fn from(factor: RateFactor) -> Self {
        factor.get()
    }
}

impl fmt::Display for RateFactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An FFT segment size of at least two samples.
///
/// Odd sizes are supported with a hop of `size / 2` samples. See the
/// [spectrum example](crate#measure-a-spectrum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FftSize(usize);

impl FftSize {
    /// Check that a half-segment hop can advance.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidFftSize`] for zero or one.
    pub const fn new(size: usize) -> Result<Self, DspError> {
        if size >= 2 {
            Ok(Self(size))
        } else {
            Err(DspError::InvalidFftSize { size })
        }
    }

    /// Samples per FFT segment.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl TryFrom<usize> for FftSize {
    type Error = DspError;
    fn try_from(size: usize) -> Result<Self, Self::Error> {
        Self::new(size)
    }
}

impl From<FftSize> for usize {
    fn from(size: FftSize) -> Self {
        size.get()
    }
}

impl fmt::Display for FftSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub(crate) fn positive(value: f32, parameter: DspParameter) -> Result<f32, DspError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(DspError::InvalidParameter {
            parameter,
            value: f64::from(value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_domains_reject_nonfinite_and_nonpositive_values() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0, -1.0] {
            assert!(SampleRate::new(value).is_err(), "rate {value}");
            assert!(Bandwidth::new(value).is_err(), "width {value}");
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Frequency::new(value).is_err(), "frequency {value}");
        }
        for count in [0, 1, 2, 4] {
            assert!(TapCount::new(count).is_err(), "count {count}");
        }
        assert!(RateFactor::new(0).is_err(), "zero factor");
        assert!(FftSize::new(0).is_err(), "zero FFT size");
        assert!(FftSize::new(1).is_err(), "one-sample FFT size");
    }

    #[test]
    fn scalar_conversions_preserve_boundary_values() -> Result<(), Box<dyn std::error::Error>> {
        let tiny = f32::from_bits(1);
        assert_eq!(
            f32::from(SampleRate::try_from(tiny)?).to_bits(),
            tiny.to_bits()
        );
        assert_eq!(
            f32::from(Bandwidth::try_from(f32::MAX)?).to_bits(),
            f32::MAX.to_bits()
        );
        assert_eq!(
            f64::from(Frequency::try_from(-f64::MAX)?).to_bits(),
            (-f64::MAX).to_bits()
        );
        assert_eq!(usize::from(TapCount::try_from(3)?), 3);
        assert_eq!(usize::from(RateFactor::try_from(usize::MAX)?), usize::MAX);
        assert_eq!(usize::from(FftSize::try_from(3)?), 3);
        let mut frequencies = std::collections::HashSet::new();
        let _inserted = frequencies.insert(Frequency::new(-0.0)?);
        assert!(
            frequencies.contains(&Frequency::new(0.0)?),
            "signed zero has one equality/hash identity"
        );
        Ok(())
    }
}
