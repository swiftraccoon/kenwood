//! The full IF-to-audio pipeline.

use crate::agc::{Agc, AgcConfig};
use crate::demod::{AmDemod, CwDemod, SsbDemod};
use crate::fir::{BandpassDesign, LowpassDesign};
use crate::nco::Nco;
use crate::resample::{Decimator, Interpolator};
use crate::{
    BASEBAND_RATE, Bandwidth, Complex32, DspError, Frequency, IF_CENTER_HZ, INPUT_RATE,
    RATE_FACTOR, RateFactor, SampleRate, TapCount,
};

/// Demodulation mode.
/// See the [configuration example](crate#configure-and-process-an-if-stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemodMode {
    /// Upper sideband.
    Usb,
    /// Lower sideband.
    Lsb,
    /// CW reception centered on a 700 Hz sidetone.
    Cw,
    /// Amplitude modulation (envelope detection).
    Am,
}

impl DemodMode {
    /// Default audio passband width for the mode, in hertz.
    #[must_use]
    pub const fn default_filter_hz(self) -> f32 {
        match self {
            Self::Usb | Self::Lsb => 2_600.0,
            Self::Cw => 500.0,
            Self::Am => 4_500.0,
        }
    }
}

impl std::fmt::Display for DemodMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb => f.write_str("USB"),
            Self::Lsb => f.write_str("LSB"),
            Self::Cw => f.write_str("CW"),
            Self::Am => f.write_str("AM"),
        }
    }
}

/// Validated mode, passband width and optional gain control.
///
/// See the [configuration example](crate#configure-and-process-an-if-stream).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelizerConfig {
    /// Demodulation mode.
    mode: DemodMode,
    /// Resolved audio passband width.
    filter_width: Bandwidth,
    /// Optional envelope-tracking stage.
    agc: Option<AgcConfig>,
}

impl ChannelizerConfig {
    /// Select a mode's default width and default AGC parameters.
    #[must_use]
    pub const fn new(mode: DemodMode) -> Self {
        Self {
            mode,
            filter_width: match Bandwidth::new(mode.default_filter_hz()) {
                Ok(width) => width,
                Err(_) => unreachable!(),
            },
            agc: Some(AgcConfig::new()),
        }
    }

    /// Selected demodulator.
    #[must_use]
    pub const fn mode(self) -> DemodMode {
        self.mode
    }

    /// Resolved audio width in hertz.
    #[must_use]
    pub const fn filter_width(self) -> Bandwidth {
        self.filter_width
    }

    /// Gain-control parameters; `None` bypasses the AGC stage.
    #[must_use]
    pub const fn agc(self) -> Option<AgcConfig> {
        self.agc
    }

    /// Override the width while keeping every mode edge within Nyquist.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCutoff`] for widths at or above 5,900 Hz in
    /// SSB, 10,600 Hz in CW, or 6,000 Hz in AM. Returns
    /// [`DspError::InvalidPassband`] when distinct edges cannot be represented.
    pub fn with_filter_width(mut self, width: Bandwidth) -> Result<Self, DspError> {
        validate_width(self.mode, width)?;
        self.filter_width = width;
        Ok(self)
    }

    /// Select validated gain parameters, or bypass the stage with `None`.
    #[must_use]
    pub const fn with_agc(mut self, config: Option<AgcConfig>) -> Self {
        self.agc = config;
        self
    }
}

impl Default for ChannelizerConfig {
    fn default() -> Self {
        Self::new(DemodMode::Usb)
    }
}

/// SSB passband low edge in hertz (keeps hum and the carrier slot out).
const SSB_LOW_EDGE_HZ: f32 = 100.0;
/// CW sidetone pitch in hertz.
const CW_PITCH_HZ: f32 = 700.0;
/// Taps for the decimation anti-alias lowpass (at the input rate).
const DECIM_TAPS: usize = 63;
/// Taps for the mode passband (at the baseband rate).
const MODE_TAPS: usize = 255;
/// Taps for the interpolation image filter (at the output rate).
const INTERP_TAPS: usize = 63;

#[derive(Clone)]
enum ModeDemod {
    Ssb(SsbDemod),
    Cw(CwDemod),
    Am(AmDemod),
}

impl std::fmt::Debug for ModeDemod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ssb(_) => f.write_str("ModeDemod::Ssb"),
            Self::Cw(_) => f.write_str("ModeDemod::Cw"),
            Self::Am(_) => f.write_str("ModeDemod::Am"),
        }
    }
}

fn validate_width(mode: DemodMode, width: Bandwidth) -> Result<(), DspError> {
    let maximum_hz = match mode {
        DemodMode::Usb | DemodMode::Lsb => {
            f64::from(BASEBAND_RATE) / 2.0 - f64::from(SSB_LOW_EDGE_HZ)
        }
        DemodMode::Cw => 2.0_f64.mul_add(-f64::from(CW_PITCH_HZ), f64::from(BASEBAND_RATE)),
        DemodMode::Am => f64::from(BASEBAND_RATE) / 2.0,
    };
    let cutoff_hz = f64::from(width.get());
    if cutoff_hz >= maximum_hz {
        return Err(DspError::InvalidCutoff {
            cutoff_hz,
            maximum_hz,
        });
    }
    let rate = SampleRate::new(BASEBAND_RATE)?;
    let taps = TapCount::new(MODE_TAPS)?;
    let (low, high) = match mode {
        DemodMode::Usb => (
            f64::from(SSB_LOW_EDGE_HZ),
            f64::from(SSB_LOW_EDGE_HZ) + cutoff_hz,
        ),
        DemodMode::Lsb => (
            -(f64::from(SSB_LOW_EDGE_HZ) + cutoff_hz),
            -f64::from(SSB_LOW_EDGE_HZ),
        ),
        DemodMode::Cw => (
            f64::from(CW_PITCH_HZ) - cutoff_hz / 2.0,
            f64::from(CW_PITCH_HZ) + cutoff_hz / 2.0,
        ),
        DemodMode::Am => return Ok(()),
    };
    let _design = BandpassDesign::new(Frequency::new(low)?..Frequency::new(high)?, rate, taps)?;
    Ok(())
}

fn build_demod(mode: DemodMode, width: Bandwidth) -> Result<ModeDemod, DspError> {
    let rate = SampleRate::new(BASEBAND_RATE)?;
    let taps = TapCount::new(MODE_TAPS)?;
    let width_hz = f64::from(width.get());
    let low = f64::from(SSB_LOW_EDGE_HZ);
    Ok(match mode {
        DemodMode::Usb => ModeDemod::Ssb(SsbDemod::new(BandpassDesign::new(
            Frequency::new(low)?..Frequency::new(low + width_hz)?,
            rate,
            taps,
        )?)?),
        DemodMode::Lsb => ModeDemod::Ssb(SsbDemod::new(BandpassDesign::new(
            Frequency::new(-(low + width_hz))?..Frequency::new(-low)?,
            rate,
            taps,
        )?)?),
        DemodMode::Cw => ModeDemod::Cw(CwDemod::new(
            Frequency::new(f64::from(CW_PITCH_HZ))?,
            width,
            rate,
            taps,
        )?),
        DemodMode::Am => ModeDemod::Am(AmDemod::new(LowpassDesign::new(
            width,
            rate,
            TapCount::new(DECIM_TAPS)?,
        )?)?),
    })
}

/// IF stream in, demodulated audio out. See the crate docs for the
/// pipeline stages and buffer conventions.
/// Cloning copies all stream history into an independent processor.
#[derive(Debug, Clone)]
pub struct Channelizer {
    nco: Nco,
    decim: Decimator,
    demod: ModeDemod,
    agc: Option<Agc>,
    interp: Interpolator,
    mode: DemodMode,
    filter_width: Bandwidth,
    mixed: Vec<Complex32>,
    baseband: Vec<Complex32>,
    audio: Vec<f32>,
}

impl Channelizer {
    /// Build the pipeline for [`INPUT_RATE`] input centered on
    /// [`IF_CENTER_HZ`].
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidAgcTiming`] for unrepresentable AGC tracking
    /// coefficients, or [`DspError::InvalidTaps`] for nonfinite designed
    /// coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(config: ChannelizerConfig) -> Result<Self, DspError> {
        let input_rate = SampleRate::new(INPUT_RATE)?;
        let factor = RateFactor::new(RATE_FACTOR)?;
        let conversion_filter = LowpassDesign::new(
            Bandwidth::new(BASEBAND_RATE / 2.0 * 0.9)?,
            input_rate,
            TapCount::new(DECIM_TAPS)?,
        )?;
        Ok(Self {
            nco: Nco::new(Frequency::new(-f64::from(IF_CENTER_HZ))?, input_rate),
            decim: Decimator::new(factor, conversion_filter)?,
            demod: build_demod(config.mode, config.filter_width)?,
            agc: config
                .agc
                .map(|c| Agc::new(c, SampleRate::new(BASEBAND_RATE)?))
                .transpose()?,
            interp: Interpolator::new(
                factor,
                LowpassDesign::new(
                    Bandwidth::new(BASEBAND_RATE / 2.0 * 0.9)?,
                    SampleRate::new(crate::OUTPUT_RATE)?,
                    TapCount::new(INTERP_TAPS)?,
                )?,
            )?,
            mode: config.mode,
            filter_width: config.filter_width,
            mixed: Vec::new(),
            baseband: Vec::new(),
            audio: Vec::new(),
        })
    }

    /// Current demodulation mode.
    #[must_use]
    pub const fn mode(&self) -> DemodMode {
        self.mode
    }

    /// Current audio passband width in hertz.
    #[must_use]
    pub const fn filter_hz(&self) -> f32 {
        self.filter_width.get()
    }

    /// Switch demodulation mode. The mode path is rebuilt with the
    /// mode's default passband; mixer, decimator, and AGC state carry
    /// over. The new mode filter starts with zero history, so its transient is
    /// present. Failure preserves every previous state value.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] for nonfinite designed coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn set_mode(&mut self, mode: DemodMode) -> Result<(), DspError> {
        let filter_width = ChannelizerConfig::new(mode).filter_width;
        let demod = build_demod(mode, filter_width)?;
        self.mode = mode;
        self.filter_width = filter_width;
        self.demod = demod;
        Ok(())
    }

    /// Override the width, or reset to the current mode's default with `None`.
    ///
    /// Replaces only mode-filter history. Rejection preserves the previous
    /// width, demodulator history, and every other stage.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCutoff`] at the mode's width limit, or
    /// [`DspError::InvalidPassband`] when distinct edges cannot be represented,
    /// or [`DspError::InvalidTaps`] for nonfinite designed coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn set_filter_width(&mut self, width: Option<Bandwidth>) -> Result<(), DspError> {
        let width = width.unwrap_or_else(|| ChannelizerConfig::new(self.mode).filter_width);
        validate_width(self.mode, width)?;
        let demod = build_demod(self.mode, width)?;
        self.filter_width = width;
        self.demod = demod;
        Ok(())
    }

    /// Process one block of IF samples into demodulated audio at the
    /// output rate. `out` is cleared first. Arbitrary block sizes are
    /// fine; stream state carries across calls.
    ///
    /// Each emitted baseband sample produces four output samples. The
    /// decimator's phase determines the count for input lengths not divisible
    /// by four, so individual blocks need not preserve their input length.
    /// Scratch and output vectors grow when needed; see
    /// [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.mixed.clear();
        for &x in input {
            self.mixed.push(self.nco.next_sample() * x);
        }
        self.decim.process(&self.mixed, &mut self.baseband);
        match &mut self.demod {
            ModeDemod::Ssb(d) => d.process(&self.baseband, &mut self.audio),
            ModeDemod::Cw(d) => d.process(&self.baseband, &mut self.audio),
            ModeDemod::Am(d) => d.process(&self.baseband, &mut self.audio),
        }
        if let Some(agc) = &mut self.agc {
            agc.process(&mut self.audio);
        }
        self.interp.process(&self.audio, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum_test_support::tone_amplitude;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Real IF tone at `IF_CENTER_HZ + offset_hz`, unit amplitude.
    fn if_tone(offset_hz: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| {
                #[expect(clippy::cast_precision_loss, reason = "small test lengths")]
                let ph = std::f64::consts::TAU * (12_000.0 + offset_hz) * n as f64 / 48_000.0;
                #[expect(clippy::cast_possible_truncation, reason = "unit tone samples")]
                let s = ph.cos() as f32;
                s
            })
            .collect()
    }

    fn no_agc(mode: DemodMode) -> Result<Channelizer, DspError> {
        Channelizer::new(ChannelizerConfig::new(mode).with_agc(None))
    }

    #[test]
    fn usb_tone_above_center_demodulates_at_offset() -> TestResult {
        let mut ch = no_agc(DemodMode::Usb)?;
        let mut out = Vec::new();
        ch.process(&if_tone(1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).ok_or("USB steady-state samples")?;
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!((amp - 1.0).abs() < 0.1, "USB amplitude {amp}, want ~1.0");
        let below = tone_amplitude(steady, 48_000.0, 2_000.0)?;
        assert!(below < 0.05, "spurious 2 kHz content {below}");
        Ok(())
    }

    #[test]
    fn usb_rejects_a_tone_below_center() -> TestResult {
        let mut ch = no_agc(DemodMode::Usb)?;
        let mut out = Vec::new();
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out
            .get(24_000..)
            .ok_or("USB rejection steady-state samples")?;
        let leak = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!(leak < 0.05, "opposite-sideband leakage {leak}");
        Ok(())
    }

    #[test]
    fn lsb_tone_below_center_demodulates_at_offset() -> TestResult {
        let mut ch = no_agc(DemodMode::Lsb)?;
        let mut out = Vec::new();
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).ok_or("LSB steady-state samples")?;
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!((amp - 1.0).abs() < 0.1, "LSB amplitude {amp}, want ~1.0");
        Ok(())
    }

    #[test]
    fn mode_and_filter_switch_midstream_preserves_other_stages() -> TestResult {
        let mut ch = no_agc(DemodMode::Usb)?;
        let mut out = Vec::new();
        ch.process(&if_tone(1_000.0, 48_000), &mut out);
        ch.set_mode(DemodMode::Lsb)?;
        assert_eq!(ch.mode(), DemodMode::Lsb);
        assert!(
            (ch.filter_hz() - DemodMode::Lsb.default_filter_hz()).abs() < f32::EPSILON,
            "mode switch resets the filter width"
        );
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out
            .get(24_000..)
            .ok_or("mode-change steady-state samples")?;
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!((amp - 1.0).abs() < 0.15, "post-switch amplitude {amp}");
        ch.set_filter_width(Some(Bandwidth::new(1_800.0)?))?;
        assert!((ch.filter_hz() - 1_800.0).abs() < f32::EPSILON);
        ch.process(&if_tone(-1_000.0, 48_000), &mut out);
        assert_eq!(out.len(), 48_000, "output length tracks input length");
        Ok(())
    }

    #[test]
    fn agc_path_produces_target_level_audio() -> TestResult {
        let mut ch = Channelizer::new(ChannelizerConfig::default())?;
        let mut out = Vec::new();
        // Quiet IF tone: AGC must lift it toward the 0.25 target.
        let quiet: Vec<f32> = if_tone(1_000.0, 144_000).iter().map(|s| s * 0.02).collect();
        ch.process(&quiet, &mut out);
        let steady = out.get(96_000..).ok_or("AGC steady-state samples")?;
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!(
            amp > 0.1 && amp < 0.4,
            "AGC output amplitude {amp}, want near 0.25"
        );
        Ok(())
    }

    #[test]
    fn mode_width_limits_and_rejected_reconfiguration_preserve_history() -> TestResult {
        for (mode, maximum) in [
            (DemodMode::Usb, 5_900.0),
            (DemodMode::Lsb, 5_900.0),
            (DemodMode::Cw, 10_600.0),
            (DemodMode::Am, 6_000.0),
        ] {
            let invalid = Bandwidth::new(maximum)?;
            assert!(
                ChannelizerConfig::new(mode)
                    .with_filter_width(invalid)
                    .is_err(),
                "mode {mode}"
            );
            let mut changed = no_agc(mode)?;
            changed.process(&if_tone(700.0, 137), &mut Vec::new());
            let mut original = changed.clone();
            assert!(
                changed.set_filter_width(Some(invalid)).is_err(),
                "invalid {mode} reconfiguration"
            );
            assert_eq!(changed.mode(), original.mode(), "mode remains unchanged");
            assert_eq!(
                changed.filter_hz().to_bits(),
                original.filter_hz().to_bits(),
                "width remains unchanged"
            );
            let input = if_tone(700.0, 129);
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            changed.process(&input, &mut actual);
            original.process(&input, &mut expected);
            assert_eq!(
                actual, expected,
                "rejection must preserve every stage's history"
            );
        }
        for mode in [DemodMode::Usb, DemodMode::Lsb, DemodMode::Cw] {
            let rejected =
                ChannelizerConfig::new(mode).with_filter_width(Bandwidth::new(f32::from_bits(1))?);
            assert!(
                matches!(rejected, Err(DspError::InvalidPassband { .. })),
                "{mode} edges must remain distinct"
            );
        }
        Ok(())
    }

    #[test]
    fn arbitrary_input_chunks_preserve_output_and_clear_empty_blocks() -> TestResult {
        let input = if_tone(1_000.0, 257);
        let mut whole = no_agc(DemodMode::Usb)?;
        let mut chunked = whole.clone();
        let mut expected = Vec::new();
        whole.process(&input, &mut expected);
        let mut actual = Vec::new();
        let mut block = Vec::new();
        for chunk in input.chunks(3) {
            chunked.process(chunk, &mut block);
            actual.extend_from_slice(&block);
            chunked.process(&[], &mut block);
            assert!(
                block.is_empty(),
                "empty input clears output without advancing state"
            );
        }
        assert_eq!(
            actual, expected,
            "block boundaries must not change the stream"
        );
        assert_eq!(
            actual.len(),
            260,
            "257 samples include 65 decimator outputs"
        );
        Ok(())
    }
}
