// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Checked public diagnostic-stage inputs.

use std::fmt;

use realfft::num_complex::Complex;

/// Nonredundant bins of a 256-point real FFT.
pub const SPECTRUM_BINS: usize = 129;
/// Samples in the encoder's pitch-estimation history.
pub const PITCH_HISTORY_SAMPLES: usize = 301;
/// Candidate pitch periods, from 21 through 122 samples in half-sample steps.
pub const PITCH_ERROR_CANDIDATES: usize = 203;

/// A diagnostic field rejected before a stage changes caller-visible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageField {
    /// Complex spectrum component or total spectral power.
    Spectrum,
    /// Pitch-history sample.
    PitchHistory,
    /// Fundamental in FFT-bin units.
    FundamentalBin,
    /// Candidate pitch period in samples.
    PitchPeriod,
    /// Confidence or complementary detectability error.
    PitchMetric,
    /// Voiced-band count or padding.
    Voicing,
    /// Harmonic count or magnitudes.
    Magnitudes,
    /// Previous-frame reconstructed log magnitudes.
    PreviousMagnitudes,
    /// Previous reconstructed log gain.
    PreviousGain,
    /// A codeword byte that must represent one bit.
    CodewordBit,
}

/// Invalid diagnostic-stage input or unrepresentable arithmetic result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StageError {
    /// A floating value was outside the field's documented finite range.
    Value {
        /// Field being checked.
        field: StageField,
        /// Zero-based component index, or zero for a scalar.
        index: usize,
        /// Rejected value.
        value: f32,
    },
    /// A count exceeded its backing array.
    Count {
        /// Field being checked.
        field: StageField,
        /// Rejected active count.
        actual: usize,
        /// Largest count accepted by the backing array.
        maximum: usize,
    },
    /// An inactive array element was nonzero.
    Padding {
        /// Field being checked.
        field: StageField,
        /// First nonzero inactive entry.
        index: usize,
    },
    /// Finite input values produced an unrepresentable stage intermediate/result.
    ArithmeticRange {
        /// Numerical stage or field.
        field: StageField,
    },
    /// A codeword contained a byte other than zero or one.
    Bit {
        /// First invalid bit position.
        index: usize,
        /// Rejected byte.
        value: u8,
    },
    /// Voicing and spectral active counts described different band layouts.
    BandCount {
        /// Active spectral harmonics.
        harmonics: usize,
        /// Bands required by that harmonic layout.
        expected: usize,
        /// Supplied voiced-band count.
        actual: usize,
    },
}

impl fmt::Display for StageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Value {
                field,
                index,
                value,
            } => write!(formatter, "invalid {field:?} value {value} at {index}"),
            Self::Count {
                field,
                actual,
                maximum,
            } => write!(formatter, "{field:?} count {actual} exceeds {maximum}"),
            Self::Padding { field, index } => {
                write!(formatter, "{field:?} padding is nonzero at {index}")
            }
            Self::ArithmeticRange { field } => {
                write!(formatter, "{field:?} arithmetic exceeds finite f32 range")
            }
            Self::Bit { index, value } => write!(
                formatter,
                "codeword bit {index} is {value}; expected zero or one"
            ),
            Self::BandCount {
                harmonics,
                expected,
                actual,
            } => write!(
                formatter,
                "{harmonics} harmonics require {expected} bands; received {actual}"
            ),
        }
    }
}

impl std::error::Error for StageError {}

/// Fixed 129-bin spectrum with finite components and finite summed squared norm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spectrum(pub(crate) [Complex<f32>; SPECTRUM_BINS]);

impl Spectrum {
    /// A zero spectrum, useful for initial diagnostic state.
    pub const ZERO: Self = Self([Complex::new(0.0, 0.0); SPECTRUM_BINS]);

    /// Validate every complex component and total power without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Value`] for nonfinite components, or
    /// [`StageError::ArithmeticRange`] when summed squared norm is nonfinite.
    pub fn new(bins: [Complex<f32>; SPECTRUM_BINS]) -> Result<Self, StageError> {
        for (index, bin) in bins.iter().enumerate() {
            for value in [bin.re, bin.im] {
                if !value.is_finite() {
                    return Err(StageError::Value {
                        field: StageField::Spectrum,
                        index,
                        value,
                    });
                }
            }
        }
        let power: f32 = bins.iter().map(Complex::norm_sqr).sum();
        if !power.is_finite() {
            return Err(StageError::ArithmeticRange {
                field: StageField::Spectrum,
            });
        }
        Ok(Self(bins))
    }

    /// Borrow the exact half-spectrum bins.
    #[must_use]
    pub const fn bins(&self) -> &[Complex<f32>; SPECTRUM_BINS] {
        &self.0
    }
}

impl TryFrom<[Complex<f32>; SPECTRUM_BINS]> for Spectrum {
    type Error = StageError;
    fn try_from(value: [Complex<f32>; SPECTRUM_BINS]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Spectrum> for [Complex<f32>; SPECTRUM_BINS] {
    fn from(value: Spectrum) -> Self {
        value.0
    }
}
impl AsRef<[Complex<f32>]> for Spectrum {
    fn as_ref(&self) -> &[Complex<f32>] {
        &self.0
    }
}

/// Borrowed finite pitch-history samples; processing can reject numerical overflow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchHistory<'a>(pub(crate) &'a [f32; PITCH_HISTORY_SAMPLES]);

impl<'a> PitchHistory<'a> {
    /// Check a complete fixed-length history without copying or allocating.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Value`] for the first nonfinite sample.
    pub fn new(samples: &'a [f32; PITCH_HISTORY_SAMPLES]) -> Result<Self, StageError> {
        for (index, &value) in samples.iter().enumerate() {
            if !value.is_finite() {
                return Err(StageError::Value {
                    field: StageField::PitchHistory,
                    index,
                    value,
                });
            }
        }
        Ok(Self(samples))
    }
    /// Borrow all pitch-history samples.
    #[must_use]
    pub const fn samples(self) -> &'a [f32; PITCH_HISTORY_SAMPLES] {
        self.0
    }
}

impl<'a> TryFrom<&'a [f32; PITCH_HISTORY_SAMPLES]> for PitchHistory<'a> {
    type Error = StageError;
    fn try_from(value: &'a [f32; PITCH_HISTORY_SAMPLES]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Finite fundamental frequency above DC and at or below bin 128 (Nyquist).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FundamentalBin(f32);

impl FundamentalBin {
    /// Validate a frequency in `(0,128]` FFT bins.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Value`] for nonfinite/out-of-range input.
    pub const fn new(value: f32) -> Result<Self, StageError> {
        if !value.is_finite() || value <= 0.0 || value > 128.0 {
            Err(StageError::Value {
                field: StageField::FundamentalBin,
                index: 0,
                value,
            })
        } else {
            Ok(Self(value))
        }
    }
    /// Return the frequency in FFT-bin units.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for FundamentalBin {
    type Error = StageError;
    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<FundamentalBin> for f32 {
    fn from(value: FundamentalBin) -> Self {
        value.get()
    }
}

/// Finite pitch confidence/detectability metric in `[0,1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchMetric(f32);

impl PitchMetric {
    pub(crate) const HALF: Self = Self(0.5);
    /// Validate an inclusive unit interval.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Value`] for a nonfinite or out-of-range value.
    pub const fn new(value: f32) -> Result<Self, StageError> {
        if !value.is_finite() || value < 0.0 || value > 1.0 {
            Err(StageError::Value {
                field: StageField::PitchMetric,
                index: 0,
                value,
            })
        } else {
            Ok(Self(value))
        }
    }
    /// Return the metric value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for PitchMetric {
    type Error = StageError;
    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<PitchMetric> for f32 {
    fn from(value: PitchMetric) -> Self {
        value.get()
    }
}

/// Validated detectability scores for every half-sample pitch candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchErrors(pub(crate) [f32; PITCH_ERROR_CANDIDATES]);

impl PitchErrors {
    /// Validate all 203 detectability scores in `[0,1]`.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Value`] with the first invalid candidate index.
    pub fn new(values: [f32; PITCH_ERROR_CANDIDATES]) -> Result<Self, StageError> {
        for (index, &value) in values.iter().enumerate() {
            if PitchMetric::new(value).is_err() {
                return Err(StageError::Value {
                    field: StageField::PitchMetric,
                    index,
                    value,
                });
            }
        }
        Ok(Self(values))
    }
    /// Borrow all candidate scores.
    #[must_use]
    pub const fn scores(&self) -> &[f32; PITCH_ERROR_CANDIDATES] {
        &self.0
    }
}

impl TryFrom<[f32; PITCH_ERROR_CANDIDATES]> for PitchErrors {
    type Error = StageError;
    fn try_from(value: [f32; PITCH_ERROR_CANDIDATES]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<PitchErrors> for [f32; PITCH_ERROR_CANDIDATES] {
    fn from(value: PitchErrors) -> Self {
        value.0
    }
}

/// FEC codeword array whose 72 bytes each represent one bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodewordBits([u8; 72]);

impl CodewordBits {
    /// Validate zero/one bit representation without changing bit order.
    ///
    /// # Errors
    ///
    /// Returns [`StageError::Bit`] for the first byte greater than one.
    pub fn new(values: [u8; 72]) -> Result<Self, StageError> {
        for (index, &value) in values.iter().enumerate() {
            if value > 1 {
                return Err(StageError::Bit { index, value });
            }
        }
        Ok(Self(values))
    }
    /// Borrow the 72 codeword bits.
    #[must_use]
    pub const fn bits(&self) -> &[u8; 72] {
        &self.0
    }
}

impl TryFrom<[u8; 72]> for CodewordBits {
    type Error = StageError;
    fn try_from(value: [u8; 72]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<CodewordBits> for [u8; 72] {
    fn from(value: CodewordBits) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EncoderBuffers, FftPlan, PcmFrame, PitchEstimate, PitchTracker, SpectralAmplitudes,
        VuvDecisions, VuvState, analyze_frame, compute_e_p, detect_vuv_and_sa, validation,
    };
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn diagnostic_scalar_and_array_domains_reject_invalid_construction() -> TestResult {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(PitchMetric::new(value).is_err());
            assert!(FundamentalBin::new(value).is_err());
            let mut bins = [Complex::new(0.0, 0.0); SPECTRUM_BINS];
            bins[128].im = value;
            assert!(matches!(
                Spectrum::new(bins),
                Err(StageError::Value { index: 128, .. })
            ));
            let mut history = [0.0; PITCH_HISTORY_SAMPLES];
            history[300] = value;
            assert!(matches!(
                PitchHistory::new(&history),
                Err(StageError::Value { index: 300, .. })
            ));
            let mut errors = [0.5; PITCH_ERROR_CANDIDATES];
            errors[202] = value;
            assert!(matches!(
                PitchErrors::new(errors),
                Err(StageError::Value { index: 202, .. })
            ));
        }
        for value in [-0.1, 1.1] {
            assert!(PitchMetric::new(value).is_err());
        }
        for value in [0.0, -1.0, 128.01] {
            assert!(FundamentalBin::new(value).is_err());
        }
        let _dc_edge = FundamentalBin::new(f32::MIN_POSITIVE)?;
        let _nyquist = FundamentalBin::new(128.0)?;
        let confidence = PitchMetric::new(1.0)?;
        assert!(PitchEstimate::new(1.99, confidence).is_err());
        let estimate = PitchEstimate::new(20.0, confidence)?;
        assert_eq!(estimate.f0_hz().to_bits(), 400.0_f32.to_bits());
        assert!(matches!(
            Spectrum::new([Complex::new(f32::MAX, 0.0); SPECTRUM_BINS]),
            Err(StageError::ArithmeticRange { .. })
        ));
        let mut codeword = [0; 72];
        codeword[71] = 2;
        assert!(matches!(
            CodewordBits::new(codeword),
            Err(StageError::Bit {
                index: 71,
                value: 2
            })
        ));
        Ok(())
    }

    #[test]
    fn active_counts_padding_and_predictor_state_are_checked() -> TestResult {
        assert!(matches!(
            VuvDecisions::new([false; 12], 13),
            Err(StageError::Count { .. })
        ));
        assert!(matches!(
            VuvDecisions::new([true; 12], 11),
            Err(StageError::Padding { index: 11, .. })
        ));
        assert!(matches!(
            SpectralAmplitudes::new([0.0; 56], 57),
            Err(StageError::Count { .. })
        ));
        assert!(matches!(
            SpectralAmplitudes::new([1.0; 56], 55),
            Err(StageError::Padding { index: 55, .. })
        ));
        assert!(matches!(
            SpectralAmplitudes::new([f32::MAX; 56], 56),
            Err(StageError::ArithmeticRange { .. })
        ));
        assert!(matches!(
            SpectralAmplitudes::new([-1.0; 56], 56),
            Err(StageError::Value { .. })
        ));
        assert!(matches!(
            validation::PrevFrameState::new([0.0; 57], 57, 0.0),
            Err(StageError::Count { .. })
        ));
        assert!(matches!(
            validation::PrevFrameState::new([f32::NAN; 57], 1, 0.0),
            Err(StageError::Value { .. })
        ));
        assert!(matches!(
            validation::PrevFrameState::new([0.0; 57], 1, f32::INFINITY),
            Err(StageError::Value { .. })
        ));
        let pitch = PitchEstimate::new(80.0, PitchMetric::new(0.5)?)?;
        let bands = VuvDecisions::new([false; 12], 1)?;
        let amplitudes = SpectralAmplitudes::new([0.0; 56], 6)?;
        assert!(matches!(
            validation::quantize(pitch, bands, &amplitudes, &validation::PrevFrameState::ZERO),
            Err(StageError::BandCount {
                expected: 2,
                actual: 1,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn checked_stages_form_an_executable_pipeline_and_reject_numeric_overflow() -> TestResult {
        let mut buffers = EncoderBuffers::new();
        let mut plan = FftPlan::new();
        let spectrum = analyze_frame(&PcmFrame::new([0.0; 160])?, &mut buffers, &mut plan);
        assert_eq!(spectrum, Spectrum::ZERO);
        let mut tracker = PitchTracker::new();
        let pitch = tracker.estimate(buffers.pitch_est_buf())?;
        let errors = compute_e_p(buffers.pitch_est_buf())?;
        let _future_pitch = tracker.estimate_with_lookahead(&errors, &errors, &errors);
        let fundamental = FundamentalBin::new(256.0 / pitch.period_samples())?;
        let (voicing, amplitudes) = detect_vuv_and_sa(
            &spectrum,
            fundamental,
            &mut VuvState::new(),
            PitchMetric::new(1.0 - pitch.confidence())?,
        )?;
        let quantized = validation::quantize(
            pitch,
            voicing,
            &amplitudes,
            &validation::PrevFrameState::ZERO,
        )?;
        assert!(quantized.data_bits().iter().all(|&bit| bit <= 1));
        assert!(quantized.previous_state().harmonics() <= 56);
        let mut unchanged = tracker;
        let excessive = [f32::MAX; PITCH_HISTORY_SAMPLES];
        assert!(matches!(
            tracker.estimate(PitchHistory::new(&excessive)?),
            Err(StageError::ArithmeticRange { .. })
        ));
        let after = tracker.estimate(buffers.pitch_est_buf())?;
        let expected = unchanged.estimate(buffers.pitch_est_buf())?;
        assert_eq!(after, expected);
        Ok(())
    }
}
