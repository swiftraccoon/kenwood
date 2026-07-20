//! Sans-io DSP for a 12 kHz low-IF audio stream.
//!
//! Input is a mono real-valued stream sampled at [`INPUT_RATE`] whose
//! passband of interest is centered on [`IF_CENTER_HZ`] (the low-IF
//! output some receivers provide over a sound-card interface). The
//! [`Channelizer`] mixes that IF to complex baseband, decimates to
//! [`BASEBAND_RATE`], applies a mode passband, demodulates (USB, LSB,
//! CW, or AM), applies AGC, and interpolates the audio back up to
//! [`OUTPUT_RATE`].
//!
//! # Sans-io discipline
//!
//! No I/O, no clocks (`Instant::now()` is never called), no threads.
//! Everything flows through explicit `process` calls. Steady-state
//! processing never allocates: output `Vec`s are caller-owned and
//! reused (each `process` clears its output before filling it), and
//! internal scratch buffers grow once to their working size.
//! Reconfiguration (mode or filter changes) may allocate — that is the
//! documented exception.
//!
//! # Amplitude convention
//!
//! A unit-amplitude IF tone demodulates to approximately unit-amplitude
//! audio before AGC: the real-to-complex mix halves amplitude, and the
//! SSB demodulator compensates with a factor of two.

pub mod agc;
pub mod channelizer;
pub mod demod;
pub mod fir;
pub mod nco;
pub mod resample;
pub mod spectrum;

pub use agc::{Agc, AgcConfig};
pub use channelizer::{Channelizer, ChannelizerConfig, DemodMode};
pub use nco::Nco;
pub use rustfft::num_complex::Complex32;
pub use spectrum::SpectrumEstimator;

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
    pub(crate) fn tone_amplitude(samples: &[f32], sample_rate: f32, freq_hz: f32) -> f32 {
        crate::spectrum::goertzel(samples, sample_rate, freq_hz)
    }
}
