//! Demodulators operating on complex baseband.
//!
//! All demodulators consume complex baseband samples (IF already mixed
//! to 0 Hz and decimated) and produce real audio at the same rate.
//! `process` clears its output before filling it.

use crate::Complex32;
use crate::fir::{FirComplexTaps, FirRealTaps, design_complex_bandpass, design_lowpass};

/// Single-sideband demodulator (filter method): select one sideband
/// with a complex bandpass, take twice the real part.
///
/// The factor of two restores the amplitude halved by the real-to-
/// complex mix upstream, so a unit-amplitude IF tone demodulates to a
/// unit-amplitude audio tone.
#[derive(Debug, Clone)]
pub struct SsbDemod {
    bpf: FirComplexTaps,
}

impl SsbDemod {
    /// Select the band `lo_hz..hi_hz`. Positive bands demodulate USB;
    /// negative bands (e.g. `-2700.0..-100.0`) demodulate LSB.
    #[must_use]
    pub fn new(lo_hz: f32, hi_hz: f32, sample_rate: f32, taps: usize) -> Self {
        Self {
            bpf: FirComplexTaps::new(design_complex_bandpass(lo_hz, hi_hz, sample_rate, taps)),
        }
    }

    /// Demodulate `input` into `out` (cleared first).
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        out.clear();
        for &x in input {
            out.push(2.0 * self.bpf.push(x).re);
        }
    }
}

/// CW demodulator: USB reception with a narrow passband centered on
/// the sidetone pitch. Tune so the carrier lands `pitch_hz` below the
/// dial and it sounds at `pitch_hz`.
#[derive(Debug, Clone)]
pub struct CwDemod {
    inner: SsbDemod,
}

impl CwDemod {
    /// Passband is `pitch_hz ± width_hz / 2`.
    #[must_use]
    pub fn new(pitch_hz: f32, width_hz: f32, sample_rate: f32, taps: usize) -> Self {
        Self {
            inner: SsbDemod::new(
                pitch_hz - width_hz / 2.0,
                pitch_hz + width_hz / 2.0,
                sample_rate,
                taps,
            ),
        }
    }

    /// Demodulate `input` into `out` (cleared first).
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        self.inner.process(input, out);
    }
}

/// AM envelope demodulator: two-sided lowpass, magnitude, single-pole
/// DC block (removes the carrier's envelope offset).
#[derive(Debug, Clone)]
pub struct AmDemod {
    lpf: FirRealTaps,
    dc: f32,
    dc_alpha: f32,
}

impl AmDemod {
    /// `cutoff_hz` bounds the audio bandwidth (one-sided).
    #[must_use]
    pub fn new(cutoff_hz: f32, sample_rate: f32, taps: usize) -> Self {
        // Single-pole DC blocker around 50 Hz.
        let dc_alpha = 1.0 - (-std::f32::consts::TAU * 50.0 / sample_rate).exp();
        Self {
            lpf: FirRealTaps::new(design_lowpass(cutoff_hz, sample_rate, taps)),
            dc: 0.0,
            dc_alpha,
        }
    }

    /// Demodulate `input` into `out` (cleared first).
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        out.clear();
        for &x in input {
            let env = self.lpf.push(x).norm();
            self.dc += self.dc_alpha * (env - self.dc);
            out.push(env - self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum_test_support::tone_amplitude;

    /// Complex tone at `freq_hz` (may be negative), amplitude 0.5 —
    /// what a unit-amplitude IF tone looks like after the real mix.
    fn half_tone(freq_hz: f64, rate: f64, len: usize) -> Vec<Complex32> {
        (0..len)
            .map(|n| {
                #[expect(clippy::cast_precision_loss, reason = "small test lengths")]
                let ph = std::f64::consts::TAU * freq_hz * n as f64 / rate;
                #[expect(clippy::cast_possible_truncation, reason = "half-unit tone samples")]
                Complex32::new((0.5 * ph.cos()) as f32, (0.5 * ph.sin()) as f32)
            })
            .collect()
    }

    #[test]
    fn usb_restores_unit_amplitude_and_rejects_the_mirror() {
        let mut usb = SsbDemod::new(100.0, 2_700.0, 12_000.0, 127);
        let mut out = Vec::new();
        usb.process(&half_tone(1_000.0, 12_000.0, 12_000), &mut out);
        let steady = out.get(2_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.05, "USB amplitude {amp}, want ~1.0");

        let mut out2 = Vec::new();
        usb.process(&half_tone(-1_000.0, 12_000.0, 12_000), &mut out2);
        let steady2 = out2.get(2_000..).unwrap_or(&[]);
        let mirror = tone_amplitude(steady2, 12_000.0, 1_000.0);
        assert!(mirror < 0.02, "mirror leakage {mirror} (want < -34 dB)");
    }

    #[test]
    fn lsb_takes_the_negative_band() {
        let mut lsb = SsbDemod::new(-2_700.0, -100.0, 12_000.0, 127);
        let mut out = Vec::new();
        lsb.process(&half_tone(-1_000.0, 12_000.0, 12_000), &mut out);
        let steady = out.get(2_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.05, "LSB amplitude {amp}, want ~1.0");
    }

    #[test]
    fn cw_narrowband_centers_on_pitch() {
        let mut cw = CwDemod::new(700.0, 500.0, 12_000.0, 255);
        let mut out = Vec::new();
        cw.process(&half_tone(700.0, 12_000.0, 24_000), &mut out);
        let steady = out.get(4_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 12_000.0, 700.0);
        assert!((amp - 1.0).abs() < 0.1, "CW amplitude {amp}, want ~1.0");
        // A tone 1 kHz off pitch is outside the 500 Hz passband.
        let mut out2 = Vec::new();
        cw.process(&half_tone(1_700.0, 12_000.0, 24_000), &mut out2);
        let steady2 = out2.get(4_000..).unwrap_or(&[]);
        let off = tone_amplitude(steady2, 12_000.0, 1_700.0);
        assert!(off < 0.05, "off-pitch leakage {off}");
    }

    #[test]
    fn am_recovers_the_modulation_tone() {
        // AM at complex baseband: carrier at 0 Hz, modulated envelope.
        // Unit carrier halved by the mix: 0.5 * (1 + 0.5 cos(2π 1k t)).
        let input: Vec<Complex32> = (0..24_000)
            .map(|n: i32| {
                let t = f64::from(n) / 12_000.0;
                let env = 0.5 * 0.5_f64.mul_add((std::f64::consts::TAU * 1_000.0 * t).cos(), 1.0);
                #[expect(clippy::cast_possible_truncation, reason = "sub-unit envelope samples")]
                Complex32::new(env as f32, 0.0)
            })
            .collect();
        let mut am = AmDemod::new(4_500.0, 12_000.0, 63);
        let mut out = Vec::new();
        am.process(&input, &mut out);
        let steady = out.get(4_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0);
        // Expected: 0.5 (mix) * 0.5 (mod index) = 0.25.
        assert!(
            (amp - 0.25).abs() < 0.05,
            "AM recovered amplitude {amp}, want ~0.25"
        );
        // DC block holds: mean of steady-state output is near zero.
        #[expect(clippy::cast_precision_loss, reason = "test-scale lengths")]
        let mean: f32 = steady.iter().sum::<f32>() / steady.len().max(1) as f32;
        assert!(mean.abs() < 0.02, "residual DC {mean}");
    }
}
