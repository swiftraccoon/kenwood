// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Checked finite, unit-scale PCM inputs.

use std::fmt;

/// Samples in one 20 ms mono frame at 8 kHz.
pub const PCM_FRAME_SAMPLES: usize = 160;

/// A PCM input rejected before any codec or enhancer state changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PcmError {
    /// A frame had a different number of samples.
    Length {
        /// Required sample count.
        expected: usize,
        /// Supplied sample count.
        actual: usize,
    },
    /// A sample was NaN or infinite.
    NonFinite {
        /// Zero-based sample position.
        index: usize,
        /// Rejected value.
        value: f32,
    },
    /// A finite sample exceeded the inclusive unit-scale range.
    OutOfRange {
        /// Zero-based sample position.
        index: usize,
        /// Rejected value.
        value: f32,
    },
}

impl fmt::Display for PcmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Length { expected, actual } => write!(
                f,
                "PCM frame has {actual} samples; exactly {expected} required"
            ),
            Self::NonFinite { index, value } => {
                write!(f, "PCM sample {index} is nonfinite: {value}")
            }
            Self::OutOfRange { index, value } => {
                write!(f, "PCM sample {index} is {value}; expected -1 through 1")
            }
        }
    }
}

impl std::error::Error for PcmError {}

/// Borrowed finite PCM samples in the inclusive range `[-1, 1]`.
///
/// Conversion checks every sample without allocating. Empty input is valid.
/// Applications choose whether to clip captured values before construction;
/// this type neither truncates buffers nor silently clips nonfinite values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcmSamples<'a>(&'a [f32]);

impl<'a> PcmSamples<'a> {
    /// Check a complete borrowed sample block before processing it.
    ///
    /// # Errors
    ///
    /// Returns [`PcmError::NonFinite`] or [`PcmError::OutOfRange`] at the first
    /// invalid sample. See the [checked PCM workflow](crate#checked-pcm).
    pub fn new(samples: &'a [f32]) -> Result<Self, PcmError> {
        for (index, &value) in samples.iter().enumerate() {
            if !value.is_finite() {
                return Err(PcmError::NonFinite { index, value });
            }
            if !(-1.0..=1.0).contains(&value) {
                return Err(PcmError::OutOfRange { index, value });
            }
        }
        Ok(Self(samples))
    }

    /// Borrow the checked samples.
    #[must_use]
    pub const fn as_slice(self) -> &'a [f32] {
        self.0
    }
}

impl<'a> TryFrom<&'a [f32]> for PcmSamples<'a> {
    type Error = PcmError;
    fn try_from(value: &'a [f32]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<[f32]> for PcmSamples<'_> {
    fn as_ref(&self) -> &[f32] {
        self.0
    }
}

/// One finite, unit-scale 160-sample PCM frame at 8 kHz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcmFrame([f32; PCM_FRAME_SAMPLES]);

impl PcmFrame {
    /// Check every sample, accepting both unit-scale endpoints.
    ///
    /// # Errors
    ///
    /// Returns the sample errors documented by [`PcmSamples::new`].
    pub fn new(samples: [f32; PCM_FRAME_SAMPLES]) -> Result<Self, PcmError> {
        let _checked = PcmSamples::new(&samples)?;
        Ok(Self(samples))
    }

    /// Borrow the exact checked frame.
    #[must_use]
    pub const fn samples(&self) -> &[f32; PCM_FRAME_SAMPLES] {
        &self.0
    }

    /// Borrow this frame as a checked sample block without another scan.
    #[must_use]
    pub const fn as_pcm_samples(&self) -> PcmSamples<'_> {
        PcmSamples(&self.0)
    }
}

impl TryFrom<[f32; PCM_FRAME_SAMPLES]> for PcmFrame {
    type Error = PcmError;
    fn try_from(value: [f32; PCM_FRAME_SAMPLES]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&[f32]> for PcmFrame {
    type Error = PcmError;
    fn try_from(value: &[f32]) -> Result<Self, Self::Error> {
        let frame = value.try_into().map_err(|_| PcmError::Length {
            expected: PCM_FRAME_SAMPLES,
            actual: value.len(),
        })?;
        Self::new(frame)
    }
}

impl From<[i16; PCM_FRAME_SAMPLES]> for PcmFrame {
    fn from(value: [i16; PCM_FRAME_SAMPLES]) -> Self {
        Self(value.map(|sample| f32::from(sample) / 32768.0))
    }
}

impl From<PcmFrame> for [f32; PCM_FRAME_SAMPLES] {
    fn from(value: PcmFrame) -> Self {
        value.0
    }
}
impl AsRef<[f32]> for PcmFrame {
    fn as_ref(&self) -> &[f32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_rejects_invalid_samples_and_lengths_before_processing() -> Result<(), PcmError> {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut samples = [0.0; PCM_FRAME_SAMPLES];
            samples[159] = value;
            assert!(matches!(
                PcmFrame::new(samples),
                Err(PcmError::NonFinite { index: 159, .. })
            ));
        }
        for value in [-1.0001, 1.0001, f32::MAX] {
            assert!(matches!(
                PcmSamples::new(&[value]),
                Err(PcmError::OutOfRange { index: 0, .. })
            ));
        }
        for length in [0, 159, 161, 320] {
            assert!(
                matches!(PcmFrame::try_from(vec![0.0;length].as_slice()), Err(PcmError::Length { actual, .. }) if actual == length)
            );
        }
        let frame = PcmFrame::new([-1.0; PCM_FRAME_SAMPLES])?;
        assert_eq!(frame.as_pcm_samples().as_slice().len(), PCM_FRAME_SAMPLES);
        let positive = PcmFrame::new([1.0; PCM_FRAME_SAMPLES])?;
        assert!(
            positive
                .samples()
                .iter()
                .all(|sample| sample.to_bits() == 1.0_f32.to_bits())
        );
        assert_eq!(PcmFrame::from([i16::MIN; PCM_FRAME_SAMPLES]), frame);
        Ok(())
    }
}
