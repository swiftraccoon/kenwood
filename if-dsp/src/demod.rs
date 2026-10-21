//! Demodulators operating on complex baseband.
//!
//! All demodulators consume complex baseband samples (IF already mixed
//! to 0 Hz and decimated) and produce real audio at the same rate.
//! `process` clears its output before filling it.

use crate::fir::{BandpassDesign, FirComplexTaps, FirRealTaps, LowpassDesign};
use crate::{Bandwidth, Complex32, DspError, Frequency, SampleRate, TapCount};

/// Single-sideband demodulator (filter method): select one sideband
/// with a complex bandpass, take twice the real part.
///
/// The factor of two restores the amplitude halved by the real-to-
/// complex mix upstream, so a unit-amplitude IF tone demodulates to a
/// unit-amplitude audio tone.
/// See the [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone)]
pub struct SsbDemod {
    bpf: FirComplexTaps,
}

impl SsbDemod {
    /// Select a checked complex passband with zero-initialized history.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] if coefficient calculation is nonfinite.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(design: BandpassDesign) -> Result<Self, DspError> {
        Ok(Self {
            bpf: FirComplexTaps::new(design.to_taps())?,
        })
    }

    /// Demodulate `input` into `out` (cleared first).
    ///
    /// Emits one audio sample per complex input and retains filter history.
    /// Output growth may allocate;
    /// [sample arithmetic](crate#allocation-and-samples) is unchecked.
    ///
    /// # Panics
    ///
    /// Standard output-vector capacity limits can panic.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        out.clear();
        for &x in input {
            out.push(2.0 * self.bpf.push(x).re);
        }
    }
}

/// CW demodulator with an adjustable passband around a sidetone pitch.
///
/// Tune so the carrier lands at the selected baseband pitch. The default
/// channelizer uses a narrow positive-frequency band; wider explicit bands
/// may cross zero while staying inside Nyquist.
/// See the [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone)]
pub struct CwDemod {
    inner: SsbDemod,
}

impl CwDemod {
    /// Passband is `pitch_hz ± width_hz / 2`.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidPassband`] when either edge reaches Nyquist
    /// or the width cannot be represented around the pitch, and
    /// [`DspError::InvalidTaps`] for nonfinite designed coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(
        pitch: Frequency,
        width: Bandwidth,
        sample_rate: SampleRate,
        taps: TapCount,
    ) -> Result<Self, DspError> {
        let half_width = f64::from(width.get()) / 2.0;
        let design = BandpassDesign::new(
            Frequency::new(pitch.get() - half_width)?..Frequency::new(pitch.get() + half_width)?,
            sample_rate,
            taps,
        )?;
        Ok(Self {
            inner: SsbDemod::new(design)?,
        })
    }

    /// Demodulate `input` into `out` (cleared first).
    ///
    /// Emits one sample per input and retains filter history. Output growth
    /// may allocate; see [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Standard output-vector capacity limits can panic.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        self.inner.process(input, out);
    }
}

/// AM envelope demodulator: two-sided lowpass, magnitude, single-pole
/// DC block (removes the carrier's envelope offset).
/// See the [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone)]
pub struct AmDemod {
    lpf: FirRealTaps,
    dc: f32,
    dc_alpha: f32,
}

impl AmDemod {
    /// Select the one-sided cutoff and a single-pole DC blocker near 50 Hz.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] for nonfinite designed coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(design: LowpassDesign) -> Result<Self, DspError> {
        // Single-pole DC blocker around 50 Hz.
        let dc_alpha = -(-std::f32::consts::TAU * 50.0 / design.sample_rate().get()).exp_m1();
        Ok(Self {
            lpf: FirRealTaps::new(design.to_taps())?,
            dc: 0.0,
            dc_alpha,
        })
    }

    /// Demodulate `input` into `out` (cleared first).
    ///
    /// Emits one sample per input and retains filter and DC-block state.
    /// Output growth may allocate; see
    /// [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Standard output-vector capacity limits can panic.
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn ssb(low: f64, high: f64) -> Result<SsbDemod, DspError> {
        SsbDemod::new(BandpassDesign::new(
            Frequency::new(low)?..Frequency::new(high)?,
            SampleRate::new(12_000.0)?,
            TapCount::new(127)?,
        )?)
    }

    /// Complex tone at `freq_hz` (may be negative), amplitude 0.5:
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
    fn usb_restores_unit_amplitude_and_rejects_the_mirror() -> TestResult {
        let mut usb = ssb(100.0, 2_700.0)?;
        let mut out = Vec::new();
        usb.process(&half_tone(1_000.0, 12_000.0, 12_000), &mut out);
        let steady = out.get(2_000..).ok_or("USB steady-state samples")?;
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0)?;
        assert!((amp - 1.0).abs() < 0.05, "USB amplitude {amp}, want ~1.0");

        let mut out2 = Vec::new();
        usb.process(&half_tone(-1_000.0, 12_000.0, 12_000), &mut out2);
        let steady2 = out2.get(2_000..).ok_or("USB mirror steady-state samples")?;
        let mirror = tone_amplitude(steady2, 12_000.0, 1_000.0)?;
        assert!(mirror < 0.02, "mirror leakage {mirror} (want < -34 dB)");
        Ok(())
    }

    #[test]
    fn lsb_takes_the_negative_band() -> TestResult {
        let mut lsb = ssb(-2_700.0, -100.0)?;
        let mut out = Vec::new();
        lsb.process(&half_tone(-1_000.0, 12_000.0, 12_000), &mut out);
        let steady = out.get(2_000..).ok_or("LSB steady-state samples")?;
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0)?;
        assert!((amp - 1.0).abs() < 0.05, "LSB amplitude {amp}, want ~1.0");
        Ok(())
    }

    #[test]
    fn cw_narrowband_centers_on_pitch() -> TestResult {
        let mut cw = CwDemod::new(
            Frequency::new(700.0)?,
            Bandwidth::new(500.0)?,
            SampleRate::new(12_000.0)?,
            TapCount::new(255)?,
        )?;
        let mut out = Vec::new();
        cw.process(&half_tone(700.0, 12_000.0, 24_000), &mut out);
        let steady = out.get(4_000..).ok_or("CW steady-state samples")?;
        let amp = tone_amplitude(steady, 12_000.0, 700.0)?;
        assert!((amp - 1.0).abs() < 0.1, "CW amplitude {amp}, want ~1.0");
        // A tone 1 kHz off pitch is outside the 500 Hz passband.
        let mut out2 = Vec::new();
        cw.process(&half_tone(1_700.0, 12_000.0, 24_000), &mut out2);
        let steady2 = out2
            .get(4_000..)
            .ok_or("CW rejection steady-state samples")?;
        let off = tone_amplitude(steady2, 12_000.0, 1_700.0)?;
        assert!(off < 0.05, "off-pitch leakage {off}");
        Ok(())
    }

    #[test]
    fn am_recovers_the_modulation_tone() -> TestResult {
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
        let mut am = AmDemod::new(LowpassDesign::new(
            Bandwidth::new(4_500.0)?,
            SampleRate::new(12_000.0)?,
            TapCount::new(63)?,
        )?)?;
        let mut out = Vec::new();
        am.process(&input, &mut out);
        let steady = out.get(4_000..).ok_or("AM steady-state samples")?;
        let amp = tone_amplitude(steady, 12_000.0, 1_000.0)?;
        // Expected: 0.5 (mix) * 0.5 (mod index) = 0.25.
        assert!(
            (amp - 0.25).abs() < 0.05,
            "AM recovered amplitude {amp}, want ~0.25"
        );
        // DC block holds: mean of steady-state output is near zero.
        #[expect(clippy::cast_precision_loss, reason = "test-scale lengths")]
        let mean: f32 = steady.iter().sum::<f32>() / steady.len().max(1) as f32;
        assert!(mean.abs() < 0.02, "residual DC {mean}");
        Ok(())
    }
}
