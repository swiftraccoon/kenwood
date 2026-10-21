#![doc = include_str!("../README.md")]

pub use agc::{Agc, AgcConfig};
pub use channelizer::{Channelizer, ChannelizerConfig, DemodMode};
pub use nco::Nco;
pub use parameters::{
    Bandwidth, DspError, DspParameter, FftSize, Frequency, RateFactor, SampleRate, TapCount,
};
pub use rustfft::num_complex::Complex32;
pub use spectrum::SpectrumEstimator;

pub mod agc;
pub mod channelizer;
pub mod demod;
pub mod fir;
pub mod nco;
mod parameters;
pub mod resample;
pub mod spectrum;

/// Sample rate of the incoming IF stream in hertz.
pub const INPUT_RATE: f32 = 48_000.0;
/// Center frequency of the low IF within the input stream, in hertz.
pub const IF_CENTER_HZ: f32 = 12_000.0;
/// Complex baseband rate after decimation, in hertz.
pub const BASEBAND_RATE: f32 = 12_000.0;
/// Output audio rate after interpolation, in hertz.
pub const OUTPUT_RATE: f32 = 48_000.0;
/// Decimation / interpolation factor between input and baseband rates.
pub const RATE_FACTOR: usize = 4;

#[cfg(test)]
pub(crate) mod spectrum_test_support {
    //! Tone amplitude measurement for unit tests.

    /// Amplitude of the `freq_hz` component of `samples`.
    pub(crate) fn tone_amplitude(
        samples: &[f32],
        sample_rate: f32,
        freq_hz: f32,
    ) -> Result<f32, crate::DspError> {
        Ok(crate::spectrum::goertzel(
            samples,
            crate::SampleRate::new(sample_rate)?,
            crate::Frequency::new(f64::from(freq_hz))?,
        ))
    }

    #[test]
    fn public_processor_state_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<crate::Agc>();
        check::<crate::Channelizer>();
        check::<crate::Nco>();
        check::<crate::SpectrumEstimator>();
        check::<crate::demod::SsbDemod>();
        check::<crate::demod::CwDemod>();
        check::<crate::demod::AmDemod>();
        check::<crate::fir::FirReal>();
        check::<crate::fir::FirRealTaps>();
        check::<crate::fir::FirComplexTaps>();
        check::<crate::resample::Decimator>();
        check::<crate::resample::Interpolator>();
        check::<crate::DspError>();
    }
}
